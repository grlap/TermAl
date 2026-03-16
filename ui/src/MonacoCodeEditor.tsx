import { useEffect, useLayoutEffect, useRef } from "react";
import type {
  IDisposable,
  editor as MonacoEditor,
} from "monaco-editor/esm/vs/editor/editor.api";
import {
  applyMonacoTheme,
  ensureMonacoEnvironment,
  monacoThemeName,
  resolveMonacoLanguage,
  type MonacoAppearance,
  type MonacoModule,
} from "./monaco";

export type MonacoCodeEditorStatus = {
  line: number;
  column: number;
  tabSize: number;
  insertSpaces: boolean;
  endOfLine: "LF" | "CRLF";
};

type MonacoCodeEditorProps = {
  appearance: MonacoAppearance;
  ariaLabel: string;
  fontSizePx: number;
  language?: string | null;
  path?: string | null;
  readOnly?: boolean;
  value: string;
  onChange?: (value: string) => void;
  onSave?: () => void;
  onStatusChange?: (status: MonacoCodeEditorStatus) => void;
};

export function MonacoCodeEditor({
  appearance,
  ariaLabel,
  fontSizePx,
  language,
  path,
  readOnly = false,
  value,
  onChange,
  onSave,
  onStatusChange,
}: MonacoCodeEditorProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const editorRef = useRef<MonacoEditor.IStandaloneCodeEditor | null>(null);
  const modelRef = useRef<MonacoEditor.ITextModel | null>(null);
  const monacoRef = useRef<MonacoModule | null>(null);
  const modelSubscriptionRef = useRef<IDisposable | null>(null);
  const cursorSubscriptionRef = useRef<IDisposable | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const changeHandlerRef = useRef(onChange);
  const saveHandlerRef = useRef(onSave);
  const statusHandlerRef = useRef(onStatusChange);
  const isApplyingExternalValueRef = useRef(false);
  const untitledUriRef = useRef(`inmemory://termal/source/${crypto.randomUUID()}`);
  const modelDescriptorRef = useRef("");

  changeHandlerRef.current = onChange;
  saveHandlerRef.current = onSave;
  statusHandlerRef.current = onStatusChange;

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

    return () => {
      resizeObserverRef.current?.disconnect();
      resizeObserverRef.current = null;
      cursorSubscriptionRef.current?.dispose();
      cursorSubscriptionRef.current = null;
      modelSubscriptionRef.current?.dispose();
      modelSubscriptionRef.current = null;
      editorRef.current?.dispose();
      editorRef.current = null;
      modelRef.current?.dispose();
      modelRef.current = null;
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

    editor.setModel(nextModel);
    previousModel?.dispose();
    modelRef.current = nextModel;
    modelDescriptorRef.current = describeModel(nextPath, nextLanguage);
    layoutEditor();
    emitStatus();
  }

  return <div ref={containerRef} className="monaco-code-editor" />;
}

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
