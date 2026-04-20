import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import type {
  IDisposable,
  editor as MonacoEditor,
} from "monaco-editor/esm/vs/editor/editor.api";
import { InlineZoneErrorBoundary } from "./InlineZoneErrorBoundary";
import {
  applyMonacoTheme,
  ensureMonacoEnvironment,
  monacoThemeName,
  resolveMonacoLanguage,
  type MonacoAppearance,
  type MonacoModule,
} from "./monaco";

/**
 * Inline rendered region used by Monaco view zones. The caller
 * produces a stable `id` so re-renders don't thrash the zone set,
 * a line number where the rendered output should appear (the zone
 * is inserted AFTER this line), and the React node to render into
 * the zone's DOM host (typically via
 * `MarkdownContent` with a synthetic fence).
 *
 * The zone's height is measured via `ResizeObserver` after mount
 * and the zone re-added with the measured height — Monaco's view
 * zone API does not support in-place height updates, so we
 * remove/re-add when content grows or shrinks. The brief flicker
 * on first paint is a deliberate trade for the simplicity of
 * React portals rendering into a plain DOM node.
 */
export type MonacoInlineZone = {
  id: string;
  afterLineNumber: number;
  render: () => ReactNode;
};

/**
 * Builds a stable structure key for the inline-zone
 * `ResizeObserver` effect's dep array. Same ids in the same order
 * produce the same string (JS `Object.is` is byte-wise for
 * primitives, so React sees the dep as unchanged and skips the
 * effect), while any id added, removed, or reordered flips the
 * key and triggers the observer to rebuild.
 *
 * Exported so the unit test can pin the "structural-equality in
 * → structural-equality out" invariant without spinning up the
 * whole MonacoCodeEditor harness. The invariant matters because
 * `inlineZoneHostState` is rewritten with a fresh array every
 * time the `inlineZones` prop changes (which happens on every
 * keystroke, since `SourcePanel`'s `renderableRegions` memo
 * depends on `editorValue`) — but the ResizeObserver only cares
 * about the set of hosts, not about who owns the latest
 * `zone.render()` closure. Keying the observer effect on this
 * derived string stops it from disconnecting and rebuilding
 * itself on every keystroke. See docs/bugs.md →
 * "`setInlineZoneHostState` writes fresh state on every
 * keystroke" for the prior symptom.
 *
 * `\n` is used as the separator because it can't appear inside a
 * zone id (region ids from `source-renderers.ts` are of the form
 * `mermaid:<start>:<end>:<hash>` or `mermaid-file:<hash>` — all
 * single-line, URL-safe). Any id with an embedded `\n` would
 * collide with ids whose boundary sits at the `\n` position,
 * but the id format makes that impossible today; this is a
 * deliberate pragmatic choice, not an injection-style concern.
 */
export function computeInlineZoneStructureKey(
  hosts: ReadonlyArray<{ id: string }>,
): string {
  return hosts.map((host) => host.id).join("\n");
}

export type MonacoCodeEditorStatus = {
  line: number;
  column: number;
  tabSize: number;
  insertSpaces: boolean;
  endOfLine: "LF" | "CRLF";
};

export type MonacoCodeEditorHandle = {
  getScrollTop: () => number;
  setScrollTop: (scrollTop: number) => void;
};

type MonacoCodeEditorProps = {
  appearance: MonacoAppearance;
  ariaLabel: string;
  fontSizePx: number;
  highlightedColumnNumber?: number | null;
  highlightedLineNumber?: number | null;
  highlightToken?: string | null;
  /**
   * Optional inline-rendered view zones keyed by stable id. When
   * supplied, the editor reserves vertical space for each zone
   * after its `afterLineNumber`, renders the React output into a
   * portal-hosted DOM node, and re-layouts whenever the content
   * height changes. Zones whose id disappears are removed; zones
   * whose `afterLineNumber` shifts are moved (via remove + re-add);
   * unchanged zones keep their position and portal continues to
   * render without remounting. Pass an empty array (or omit) to
   * use Monaco without inline renders.
   */
  inlineZones?: MonacoInlineZone[];
  language?: string | null;
  path?: string | null;
  readOnly?: boolean;
  value: string;
  onChange?: (value: string) => void;
  onSave?: () => void;
  onStatusChange?: (status: MonacoCodeEditorStatus) => void;
};

const DEFAULT_INLINE_ZONES: MonacoInlineZone[] = [];

export const MonacoCodeEditor = forwardRef<MonacoCodeEditorHandle, MonacoCodeEditorProps>(function MonacoCodeEditor({
  appearance,
  ariaLabel,
  fontSizePx,
  highlightedColumnNumber = null,
  highlightedLineNumber = null,
  highlightToken = null,
  inlineZones = DEFAULT_INLINE_ZONES,
  language,
  path,
  readOnly = false,
  value,
  onChange,
  onSave,
  onStatusChange,
}, ref) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const editorRef = useRef<MonacoEditor.IStandaloneCodeEditor | null>(null);
  const modelRef = useRef<MonacoEditor.ITextModel | null>(null);
  const monacoRef = useRef<MonacoModule | null>(null);
  const modelSubscriptionRef = useRef<IDisposable | null>(null);
  const cursorSubscriptionRef = useRef<IDisposable | null>(null);
  const keyDownSubscriptionRef = useRef<IDisposable | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const changeHandlerRef = useRef(onChange);
  const saveHandlerRef = useRef(onSave);
  const statusHandlerRef = useRef(onStatusChange);
  const readOnlyRef = useRef(readOnly);
  const isApplyingExternalValueRef = useRef(false);
  const untitledUriRef = useRef(`inmemory://termal/source/${crypto.randomUUID()}`);
  const modelDescriptorRef = useRef("");
  const highlightDecorationIdsRef = useRef<string[]>([]);
  const lastHighlightDescriptorRef = useRef("");
  // Inline view-zone state. `inlineZoneHostsRef` is the source of
  // truth for the editor-side zone registry (zone id → DOM node +
  // last-known line number + last-known height). The React state
  // mirror (`inlineZoneHostState`) triggers portal rerenders when
  // the zone set changes — using only the ref would mean React
  // never knows to render new portals.
  // Each zone owns TWO DOM nodes:
  //   - `outerNode`: what Monaco positions inside its view-zone
  //     layer. Monaco freezes this element's height at the value
  //     we pass to `addZone({ heightInPx })`. Measuring it with a
  //     ResizeObserver would just report the frozen height — it
  //     never reflects the diagram's actual content extent.
  //   - `innerNode`: the React portal target. `height: auto`, so
  //     it grows to fit the Mermaid iframe / KaTeX output.
  //     `ResizeObserver` on this node fires whenever the rendered
  //     content's size settles (async Mermaid render, user-edited
  //     fence body) and we re-add the zone with the matched height.
  // The outer node has `overflow: hidden` so the brief moment
  // between initial add and first measured re-add does not spill
  // content over the code below.
  const inlineZoneHostsRef = useRef<
    Map<
      string,
      {
        zoneId: string;
        outerNode: HTMLDivElement;
        innerNode: HTMLDivElement;
        afterLineNumber: number;
        lastHeightPx: number;
      }
    >
  >(new Map());
  const [inlineZoneHostState, setInlineZoneHostState] = useState<
    Array<{ id: string; node: HTMLDivElement; zone: MonacoInlineZone }>
  >([]);
  const inlineZoneResizeObserverRef = useRef<ResizeObserver | null>(null);

  changeHandlerRef.current = onChange;
  saveHandlerRef.current = onSave;
  statusHandlerRef.current = onStatusChange;
  readOnlyRef.current = readOnly;

  useImperativeHandle(ref, () => ({
    getScrollTop: () => editorRef.current?.getScrollTop() ?? 0,
    setScrollTop: (scrollTop: number) => {
      editorRef.current?.setScrollTop(scrollTop);
    },
  }), []);

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }

    const monaco = ensureMonacoEnvironment();
    monacoRef.current = monaco;
    syncTheme(monaco);

    const editor = monaco.editor.create(container, {
      ariaLabel,
      automaticLayout: true,
      bracketPairColorization: { enabled: true },
      fontFamily: resolveEditorFontFamily(),
      fontLigatures: true,
      fontSize: fontSizePx,
      guides: {
        bracketPairs: true,
        bracketPairsHorizontal: "active",
        highlightActiveBracketPair: true,
        highlightActiveIndentation: "always",
        indentation: true,
      },
      insertSpaces: true,
      lineNumbersMinChars: 4,
      matchBrackets: "always",
      minimap: { enabled: false },
      occurrencesHighlight: "singleFile",
      padding: { top: 8, bottom: 8 },
      readOnly,
      roundedSelection: false,
      scrollBeyondLastLine: false,
      stickyScroll: { enabled: false },
      tabSize: 2,
      wordWrap: "off",
    });
    editorRef.current = editor;

    cursorSubscriptionRef.current = editor.onDidChangeCursorPosition(() => {
      emitStatus();
    });

    resizeObserverRef.current = new ResizeObserver(() => {
      window.requestAnimationFrame(layoutEditor);
    });
    resizeObserverRef.current.observe(container);
    if (container.parentElement) {
      resizeObserverRef.current.observe(container.parentElement);
    }

    window.requestAnimationFrame(() => {
      layoutEditor();
      emitStatus();
      window.requestAnimationFrame(() => {
        layoutEditor();
        emitStatus();
      });
    });

    replaceModel(value, path, language);
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
      if (!readOnly) {
        saveHandlerRef.current?.();
      }
    });

    // Down-arrow at end-of-file appends a new empty line so the
    // cursor can move past the last content line. Monaco's default
    // behavior at EOF is a no-op (cursor just collapses to the end
    // of the last line), which leaves the user stuck when the file
    // ends mid-content — common when the last line of a document
    // is inside a fenced code block whose closing `\`\`\`` is below
    // an inline view zone (rendered Mermaid / KaTeX). Only fires
    // when the cursor is already sitting at the column-end of the
    // final line with no selection and no modifier keys, so normal
    // within-line Down-arrow behavior (collapse selection, jump to
    // end of line from a shorter line below) is preserved.
    keyDownSubscriptionRef.current = editor.onKeyDown((event) => {
      if (event.keyCode !== monaco.KeyCode.DownArrow) {
        return;
      }
      if (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) {
        return;
      }
      if (readOnlyRef.current) {
        return;
      }
      const model = editor.getModel();
      if (!model) {
        return;
      }
      const selection = editor.getSelection();
      if (!selection || !selection.isEmpty()) {
        return;
      }
      const lastLine = model.getLineCount();
      const endCol = model.getLineMaxColumn(lastLine);
      if (selection.positionLineNumber !== lastLine || selection.positionColumn !== endCol) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      editor.executeEdits("termal-down-at-eof", [
        {
          range: new monaco.Range(lastLine, endCol, lastLine, endCol),
          text: "\n",
          forceMoveMarkers: true,
        },
      ]);
      editor.setPosition({ lineNumber: lastLine + 1, column: 1 });
      editor.revealPosition({ lineNumber: lastLine + 1, column: 1 });
    });

    return () => {
      resizeObserverRef.current?.disconnect();
      resizeObserverRef.current = null;
      cursorSubscriptionRef.current?.dispose();
      cursorSubscriptionRef.current = null;
      keyDownSubscriptionRef.current?.dispose();
      keyDownSubscriptionRef.current = null;
      modelSubscriptionRef.current?.dispose();
      modelSubscriptionRef.current = null;
      inlineZoneResizeObserverRef.current?.disconnect();
      inlineZoneResizeObserverRef.current = null;
      // Inline zone DOM nodes are owned by Monaco (via addZone).
      // Disposing the editor tears them down automatically; we
      // just drop our ref map so a remount doesn't reuse stale
      // zone ids.
      inlineZoneHostsRef.current.clear();
      clearHighlightDecorations();
      editorRef.current?.dispose();
      editorRef.current = null;
      modelRef.current?.dispose();
      modelRef.current = null;
      monacoRef.current = null;
      modelDescriptorRef.current = "";
      lastHighlightDescriptorRef.current = "";
    };
  }, []);

  useEffect(() => {
    const monaco = monacoRef.current;
    if (!monaco) {
      return;
    }

    syncTheme(monaco);
    layoutEditor();
    emitStatus();
  }, [appearance]);

  // Inline view zones: add/move/remove to match the current
  // `inlineZones` prop. A stable `id` keeps the DOM node (and its
  // React portal) across re-renders so the rendered Mermaid diagram
  // does NOT unmount and re-render on every keystroke. The zone is
  // only removed-and-re-added when `afterLineNumber` actually
  // changes (Monaco lacks an "update zone position" API), and the
  // portal re-runs only if the React element identity changes.
  useEffect(() => {
    const editor = editorRef.current;
    const monaco = monacoRef.current;
    if (!editor || !monaco) {
      return;
    }

    const nextIds = new Set(inlineZones.map((zone) => zone.id));
    const hosts = inlineZoneHostsRef.current;

    editor.changeViewZones((accessor) => {
      // Drop zones whose id has disappeared from the prop. The
      // outer node is owned by Monaco — removeZone detaches it
      // from the view-zone layer on its own. The inner node is a
      // child of the outer node, so it disappears with it; no
      // explicit remove() is needed (and calling .remove() on a
      // node Monaco has already detached would throw).
      for (const [id, host] of hosts.entries()) {
        if (!nextIds.has(id)) {
          accessor.removeZone(host.zoneId);
          hosts.delete(id);
        }
      }

      // Add new zones, and re-add existing zones whose
      // `afterLineNumber` shifted (e.g. the user added lines above
      // the fence). Zones with just a React-element identity
      // change (same id + same line) are left alone so the portal
      // content updates in place without removing the zone.
      for (const zone of inlineZones) {
        const existing = hosts.get(zone.id);
        if (existing && existing.afterLineNumber === zone.afterLineNumber) {
          continue;
        }
        // Preserve the outer + inner nodes across line-number
        // changes so the portal content (and any async Mermaid
        // iframe state) survives the remove-and-re-add.
        let outerNode = existing?.outerNode;
        let innerNode = existing?.innerNode;
        if (existing) {
          accessor.removeZone(existing.zoneId);
          hosts.delete(zone.id);
        }
        if (!outerNode || !innerNode) {
          outerNode = document.createElement("div");
          outerNode.className = "monaco-inline-render-zone";
          // Clip during the brief window before the first
          // ResizeObserver tick so unmeasured content does not
          // spill over the code below.
          outerNode.style.overflow = "hidden";
          innerNode = document.createElement("div");
          innerNode.className = "monaco-inline-render-zone-content";
          // `height: auto` is the default on a div, but spelling
          // it out here pins the contract that ResizeObserver
          // measures the CONTENT size of this inner box.
          innerNode.style.height = "auto";
          outerNode.appendChild(innerNode);
        }
        const initialHeight = existing?.lastHeightPx ?? 40;
        const zoneId = accessor.addZone({
          afterLineNumber: zone.afterLineNumber,
          heightInPx: initialHeight,
          domNode: outerNode,
        });
        hosts.set(zone.id, {
          zoneId,
          outerNode,
          innerNode,
          afterLineNumber: zone.afterLineNumber,
          lastHeightPx: initialHeight,
        });
      }
    });

    // Publish the current zone set to React state so portals render
    // into the INNER node (which sizes to content). Monaco owns the
    // outer node's layout.
    setInlineZoneHostState(
      inlineZones.map((zone) => ({
        id: zone.id,
        node:
          hosts.get(zone.id)?.innerNode ?? document.createElement("div"),
        zone,
      })),
    );
  }, [inlineZones]);

  // Stable structure key for the `ResizeObserver` effect below.
  // See `computeInlineZoneStructureKey` for the contract and why
  // the observer only cares about the SET of ids, not about who
  // owns the latest `zone.render()` closure.
  const inlineZoneStructureKey = useMemo(
    () => computeInlineZoneStructureKey(inlineZoneHostState),
    [inlineZoneHostState],
  );

  // ResizeObserver watches each zone's INNER content wrapper — the
  // `height: auto` node the React portal renders into. When a
  // diagram finishes its async Mermaid render or the user edits
  // the fence body to change the diagram size, the inner node's
  // height changes; we then remove-and-re-add the outer Monaco
  // zone with the matched `heightInPx` so the editor's layout
  // reserves exactly the right amount of vertical space.
  //
  // Watching the OUTER node would be a dead end: Monaco freezes
  // its height at the `heightInPx` we pass to `addZone`, so its
  // size never reflects the true content extent. That's the root
  // cause of the "diagram overlaps source below" symptom the
  // previous implementation produced.
  //
  // Deps: `inlineZoneStructureKey` (see above) rather than
  // `inlineZoneHostState` so the observer only rebuilds when
  // the set of zone ids changes.
  useEffect(() => {
    const editor = editorRef.current;
    const monaco = monacoRef.current;
    if (!editor || !monaco || inlineZoneHostState.length === 0) {
      inlineZoneResizeObserverRef.current?.disconnect();
      inlineZoneResizeObserverRef.current = null;
      return;
    }

    const observer = new ResizeObserver((entries) => {
      const editorAlive = editorRef.current;
      if (!editorAlive) {
        return;
      }
      const hosts = inlineZoneHostsRef.current;
      const pendingUpdates: Array<{ id: string; heightPx: number }> = [];
      for (const entry of entries) {
        const target = entry.target as HTMLDivElement;
        for (const [id, host] of hosts.entries()) {
          if (host.innerNode !== target) {
            continue;
          }
          const nextHeight = Math.max(
            Math.ceil(entry.contentRect.height),
            40,
          );
          if (Math.abs(nextHeight - host.lastHeightPx) < 2) {
            continue;
          }
          pendingUpdates.push({ id, heightPx: nextHeight });
        }
      }
      if (pendingUpdates.length === 0) {
        return;
      }
      editorAlive.changeViewZones((accessor) => {
        for (const update of pendingUpdates) {
          const host = hosts.get(update.id);
          if (!host) {
            continue;
          }
          accessor.removeZone(host.zoneId);
          host.lastHeightPx = update.heightPx;
          host.zoneId = accessor.addZone({
            afterLineNumber: host.afterLineNumber,
            heightInPx: update.heightPx,
            domNode: host.outerNode,
          });
        }
      });
    });

    for (const host of inlineZoneHostsRef.current.values()) {
      observer.observe(host.innerNode);
    }
    inlineZoneResizeObserverRef.current = observer;

    return () => {
      observer.disconnect();
      if (inlineZoneResizeObserverRef.current === observer) {
        inlineZoneResizeObserverRef.current = null;
      }
    };
  }, [inlineZoneStructureKey]);

  useEffect(() => {
    const monaco = monacoRef.current;
    if (!monaco || typeof document === "undefined" || typeof MutationObserver === "undefined") {
      return;
    }

    const observer = new MutationObserver((mutations) => {
      if (!mutations.some((mutation) => mutation.attributeName === "data-theme")) {
        return;
      }

      syncTheme(monaco);
      layoutEditor();
      emitStatus();
    });

    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });

    return () => observer.disconnect();
  }, [appearance]);

  useEffect(() => {
    editorRef.current?.updateOptions({ readOnly });
    layoutEditor();
    emitStatus();
  }, [readOnly]);

  useEffect(() => {
    editorRef.current?.updateOptions({ fontSize: fontSizePx });
    layoutEditor();
    emitStatus();
  }, [fontSizePx]);

  useEffect(() => {
    const nextDescriptor = describeModel(path, language);
    if (modelDescriptorRef.current !== nextDescriptor) {
      replaceModel(modelRef.current?.getValue() ?? value, path, language);
      return;
    }

    layoutEditor();
    emitStatus();
  }, [language, path, value]);

  useEffect(() => {
    const model = modelRef.current;
    if (!model || model.getValue() === value) {
      return;
    }

    isApplyingExternalValueRef.current = true;
    model.setValue(value);
    isApplyingExternalValueRef.current = false;
    layoutEditor();
    emitStatus();
  }, [value]);

  useEffect(() => {
    const editor = editorRef.current;
    const model = modelRef.current;
    const monaco = monacoRef.current;
    if (!editor || !model || !monaco) {
      return;
    }

    if (!highlightedLineNumber) {
      clearHighlightDecorations();
      lastHighlightDescriptorRef.current = "";
      return;
    }

    const targetLineNumber = Math.min(Math.max(highlightedLineNumber, 1), model.getLineCount());
    const targetColumnNumber = Math.min(
      Math.max(highlightedColumnNumber ?? 1, 1),
      model.getLineMaxColumn(targetLineNumber),
    );
    const highlightDescriptor = [
      path?.trim() ?? "",
      highlightToken ?? "focus",
      String(targetLineNumber),
      String(targetColumnNumber),
    ].join(":");
    if (
      lastHighlightDescriptorRef.current === highlightDescriptor &&
      highlightDecorationIdsRef.current.length > 0
    ) {
      return;
    }

    lastHighlightDescriptorRef.current = highlightDescriptor;
    highlightDecorationIdsRef.current = editor.deltaDecorations(highlightDecorationIdsRef.current, [
      {
        range: new monaco.Range(
          targetLineNumber,
          1,
          targetLineNumber,
          Math.max(model.getLineMaxColumn(targetLineNumber), 1),
        ),
        options: {
          className: "monaco-link-target-line",
          isWholeLine: true,
          linesDecorationsClassName: "monaco-link-target-line-gutter",
          stickiness: monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges,
        },
      },
    ]);

    const position = {
      lineNumber: targetLineNumber,
      column: targetColumnNumber,
    };
    editor.setPosition(position);
    editor.revealPositionInCenter(position);
    emitStatus();
  }, [highlightedColumnNumber, highlightedLineNumber, highlightToken, path, value]);

  function syncTheme(monacoModule: MonacoModule) {
    applyMonacoTheme(monacoModule, appearance);
    monacoModule.editor.setTheme(monacoThemeName(appearance));
  }

  function layoutEditor() {
    const editor = editorRef.current;
    const container = containerRef.current;
    if (!editor || !container) {
      return;
    }

    const width = container.clientWidth;
    const height = container.clientHeight;
    if (width <= 0 || height <= 0) {
      return;
    }

    editor.layout({ width, height });
  }

  function emitStatus() {
    const editor = editorRef.current;
    const model = modelRef.current;
    if (!editor || !model) {
      return;
    }

    const position = editor.getPosition();
    const options = model.getOptions();
    statusHandlerRef.current?.({
      line: position?.lineNumber ?? 1,
      column: position?.column ?? 1,
      tabSize: options.tabSize,
      insertSpaces: options.insertSpaces,
      endOfLine: model.getEOL() === "\r\n" ? "CRLF" : "LF",
    });
  }

  function clearHighlightDecorations() {
    const editor = editorRef.current;
    if (!editor) {
      highlightDecorationIdsRef.current = [];
      return;
    }

    highlightDecorationIdsRef.current = editor.deltaDecorations(highlightDecorationIdsRef.current, []);
  }

  function replaceModel(nextValue: string, nextPath?: string | null, nextLanguage?: string | null) {
    const monaco = monacoRef.current;
    const editor = editorRef.current;
    if (!monaco || !editor) {
      return;
    }

    const previousModel = modelRef.current;
    const nextUri = buildModelUri(monaco, nextPath, untitledUriRef.current);
    const existingModel = monaco.editor.getModel(nextUri);
    if (existingModel && existingModel !== previousModel) {
      existingModel.dispose();
    }

    const nextModel = monaco.editor.createModel(
      nextValue,
      resolveMonacoLanguage(nextLanguage, nextPath),
      nextUri,
    );

    modelSubscriptionRef.current?.dispose();
    modelSubscriptionRef.current = nextModel.onDidChangeContent(() => {
      if (!isApplyingExternalValueRef.current) {
        changeHandlerRef.current?.(nextModel.getValue());
      }

      emitStatus();
    });

    clearHighlightDecorations();
    editor.setModel(nextModel);
    previousModel?.dispose();
    modelRef.current = nextModel;
    modelDescriptorRef.current = describeModel(nextPath, nextLanguage);
    layoutEditor();
    emitStatus();
  }

  return (
    <>
      <div ref={containerRef} className="monaco-code-editor" />
      {/* Inline zones: each host DOM node lives inside Monaco's
          view-zone slot (managed by the editor). The React portal
          renders the caller-provided content into that slot. Keyed
          by zone id so the portal stays mounted across re-renders
          when only the line number or height changes.

          The `InlineZoneErrorBoundary` catches render errors from
          each zone's subtree in isolation, so a malformed Mermaid
          fence or a KaTeX parse escape can no longer unmount
          MonacoCodeEditor (and lose the user's unsaved buffer).
          See `docs/bugs.md` → "Missing error boundary around
          portal render() in MonacoCodeEditor". The boundary
          resets itself on `zoneId` change so a new zone gets a
          clean render attempt. */}
      {inlineZoneHostState.map((host) =>
        createPortal(
          <InlineZoneErrorBoundary zoneId={host.id}>
            {host.zone.render()}
          </InlineZoneErrorBoundary>,
          host.node,
          host.id,
        ),
      )}
    </>
  );
});

function buildModelUri(monaco: MonacoModule, path: string | null | undefined, baseUri: string) {
  const normalizedPath = path?.trim();
  if (!normalizedPath) {
    return monaco.Uri.parse(`${baseUri}/untitled.txt`);
  }

  const segments = normalizedPath.split(/[\\/]+/).filter(Boolean);
  const fileName = segments[segments.length - 1] ?? "file.txt";
  return monaco.Uri.parse(`${baseUri}/${encodeURIComponent(fileName)}?path=${encodeURIComponent(normalizedPath)}`);
}

function describeModel(path: string | null | undefined, language: string | null | undefined) {
  return `${path?.trim() ?? "untitled"}::${language ?? "plaintext"}`;
}

function resolveEditorFontFamily() {
  const fallback = '"Fira Code", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace';
  if (typeof window === "undefined") {
    return fallback;
  }

  const configured = window.getComputedStyle(document.documentElement).getPropertyValue("--code-font").trim();
  return configured ? `"Fira Code", ${configured}` : fallback;
}
