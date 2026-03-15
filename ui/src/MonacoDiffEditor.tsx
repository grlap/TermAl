import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
} from "react";
import type { IDisposable, editor as MonacoEditor } from "monaco-editor/esm/vs/editor/editor.api";
import {
  applyMonacoTheme,
  ensureMonacoEnvironment,
  monacoThemeName,
  resolveMonacoLanguage,
  type MonacoAppearance,
  type MonacoModule,
} from "./monaco";
import type { MonacoCodeEditorStatus } from "./MonacoCodeEditor";

type MonacoDiffEditorProps = {
  appearance: MonacoAppearance;
  ariaLabel: string;
  language?: string | null;
  onStatusChange?: (status: MonacoDiffEditorStatus) => void;
  path?: string | null;
  modifiedValue: string;
  originalValue: string;
};

export type MonacoDiffEditorStatus = MonacoCodeEditorStatus & {
  changeCount: number;
  currentChange: number;
};

export type MonacoDiffEditorHandle = {
  goToNextChange: () => void;
  goToPreviousChange: () => void;
};

export const MonacoDiffEditor = forwardRef<MonacoDiffEditorHandle, MonacoDiffEditorProps>(function MonacoDiffEditor({
  appearance,
  ariaLabel,
  language,
  onStatusChange,
  path,
  modifiedValue,
  originalValue,
}, ref) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const diffEditorRef = useRef<MonacoEditor.IStandaloneDiffEditor | null>(null);
  const monacoRef = useRef<MonacoModule | null>(null);
  const originalModelRef = useRef<MonacoEditor.ITextModel | null>(null);
  const modifiedModelRef = useRef<MonacoEditor.ITextModel | null>(null);
  const modifiedCursorSubscriptionRef = useRef<IDisposable | null>(null);
  const diffSubscriptionRef = useRef<IDisposable | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const untitledUriRef = useRef(`inmemory://termal-diff/${crypto.randomUUID()}`);
  const modelDescriptorRef = useRef("");
  const statusHandlerRef = useRef(onStatusChange);

  statusHandlerRef.current = onStatusChange;

  useImperativeHandle(ref, () => ({
    goToNextChange() {
      diffEditorRef.current?.goToDiff("next");
      emitStatus();
    },
    goToPreviousChange() {
      diffEditorRef.current?.goToDiff("previous");
      emitStatus();
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

    const editor = monaco.editor.createDiffEditor(container, {
      ariaLabel,
      automaticLayout: true,
      bracketPairColorization: { enabled: true },
      diffCodeLens: false,
      diffWordWrap: "off",
      fontFamily: resolveEditorFontFamily(),
      fontSize: 13,
      guides: {
        bracketPairs: true,
        bracketPairsHorizontal: "active",
        highlightActiveBracketPair: true,
        highlightActiveIndentation: "always",
        indentation: true,
      },
      hideUnchangedRegions: {
        enabled: false,
      },
      ignoreTrimWhitespace: false,
      lineNumbersMinChars: 4,
      matchBrackets: "always",
      minimap: {
        enabled: false,
      },
      originalEditable: false,
      padding: { top: 8, bottom: 8 },
      readOnly: true,
      renderIndicators: true,
      renderSideBySide: true,
      renderWhitespace: "all",
      roundedSelection: false,
      scrollBeyondLastLine: false,
      stickyScroll: { enabled: false },
      useInlineViewWhenSpaceIsLimited: false,
      wordWrap: "off",
    });
    diffEditorRef.current = editor;
    modifiedCursorSubscriptionRef.current = editor.getModifiedEditor().onDidChangeCursorPosition(() => {
      emitStatus();
    });
    diffSubscriptionRef.current = editor.onDidUpdateDiff(() => {
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

    replaceModels(originalValue, modifiedValue, path, language);

    return () => {
      resizeObserverRef.current?.disconnect();
      resizeObserverRef.current = null;
      modifiedCursorSubscriptionRef.current?.dispose();
      modifiedCursorSubscriptionRef.current = null;
      diffSubscriptionRef.current?.dispose();
      diffSubscriptionRef.current = null;
      diffEditorRef.current?.dispose();
      diffEditorRef.current = null;
      originalModelRef.current?.dispose();
      originalModelRef.current = null;
      modifiedModelRef.current?.dispose();
      modifiedModelRef.current = null;
      monacoRef.current = null;
      modelDescriptorRef.current = "";
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
    const nextDescriptor = describeModel(path, language);
    if (modelDescriptorRef.current !== nextDescriptor) {
      replaceModels(originalValue, modifiedValue, path, language);
      return;
    }

    layoutEditor();
    emitStatus();
  }, [language, modifiedValue, originalValue, path]);

  useEffect(() => {
    const originalModel = originalModelRef.current;
    if (originalModel && originalModel.getValue() !== originalValue) {
      originalModel.setValue(originalValue);
    }

    const modifiedModel = modifiedModelRef.current;
    if (modifiedModel && modifiedModel.getValue() !== modifiedValue) {
      modifiedModel.setValue(modifiedValue);
    }

    layoutEditor();
    emitStatus();
  }, [modifiedValue, originalValue]);

  function syncTheme(monacoModule: MonacoModule) {
    applyMonacoTheme(monacoModule, appearance);
    monacoModule.editor.setTheme(monacoThemeName(appearance));
  }

  function layoutEditor() {
    const editor = diffEditorRef.current;
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
    const editor = diffEditorRef.current;
    const model = modifiedModelRef.current;
    if (!editor || !model) {
      return;
    }

    const modifiedEditor = editor.getModifiedEditor();
    const position = modifiedEditor.getPosition();
    const options = model.getOptions();
    const lineChanges = editor.getLineChanges() ?? [];

    statusHandlerRef.current?.({
      line: position?.lineNumber ?? 1,
      column: position?.column ?? 1,
      tabSize: options.tabSize,
      insertSpaces: options.insertSpaces,
      endOfLine: model.getEOL() === "\r\n" ? "CRLF" : "LF",
      changeCount: lineChanges.length,
      currentChange: getCurrentChangeIndex(lineChanges, position?.lineNumber ?? 1),
    });
  }

  function replaceModels(
    nextOriginalValue: string,
    nextModifiedValue: string,
    nextPath?: string | null,
    nextLanguage?: string | null,
  ) {
    const monaco = monacoRef.current;
    const editor = diffEditorRef.current;
    if (!monaco || !editor) {
      return;
    }

    const originalUri = buildModelUri(monaco, untitledUriRef.current, nextPath, "original");
    const modifiedUri = buildModelUri(monaco, untitledUriRef.current, nextPath, "modified");
    monaco.editor.getModel(originalUri)?.dispose();
    monaco.editor.getModel(modifiedUri)?.dispose();

    const originalModel = monaco.editor.createModel(
      nextOriginalValue,
      resolveMonacoLanguage(nextLanguage, nextPath),
      originalUri,
    );
    const modifiedModel = monaco.editor.createModel(
      nextModifiedValue,
      resolveMonacoLanguage(nextLanguage, nextPath),
      modifiedUri,
    );

    editor.setModel({
      original: originalModel,
      modified: modifiedModel,
    });

    originalModelRef.current?.dispose();
    modifiedModelRef.current?.dispose();
    originalModelRef.current = originalModel;
    modifiedModelRef.current = modifiedModel;
    modelDescriptorRef.current = describeModel(nextPath, nextLanguage);
    layoutEditor();
    emitStatus();
  }

  return <div ref={containerRef} className="monaco-diff-editor" />;
});

function buildModelUri(
  monaco: MonacoModule,
  baseUri: string,
  path: string | null | undefined,
  role: "original" | "modified",
) {
  const suffix = path?.trim().split(/[\\/]+/).filter(Boolean).join("-") ?? "untitled";
  return monaco.Uri.parse(`${baseUri}/${role}/${encodeURIComponent(suffix)}`);
}

function describeModel(path: string | null | undefined, language: string | null | undefined) {
  return `${path?.trim() ?? "untitled"}::${language ?? "plaintext"}`;
}

function resolveEditorFontFamily() {
  if (typeof window === "undefined") {
    return "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
  }

  const configured = window.getComputedStyle(document.documentElement).getPropertyValue("--code-font").trim();
  return configured || "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
}

function getCurrentChangeIndex(lineChanges: MonacoEditor.ILineChange[], lineNumber: number) {
  if (lineChanges.length === 0) {
    return 0;
  }

  for (let index = 0; index < lineChanges.length; index += 1) {
    const change = lineChanges[index];
    const start = resolveModifiedLineStart(change);
    const end = resolveModifiedLineEnd(change, start);

    if (lineNumber >= start && lineNumber <= end) {
      return index + 1;
    }

    if (lineNumber < start) {
      return index + 1;
    }
  }

  return lineChanges.length;
}

function resolveModifiedLineStart(change: MonacoEditor.ILineChange) {
  if (change.modifiedStartLineNumber > 0) {
    return change.modifiedStartLineNumber;
  }

  if (change.modifiedEndLineNumber > 0) {
    return change.modifiedEndLineNumber;
  }

  return 1;
}

function resolveModifiedLineEnd(change: MonacoEditor.ILineChange, fallbackStart: number) {
  if (change.modifiedEndLineNumber > 0) {
    return change.modifiedEndLineNumber;
  }

  return fallbackStart;
}