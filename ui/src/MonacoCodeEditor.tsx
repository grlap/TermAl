import { useEffect, useLayoutEffect, useRef } from "react";
import type {
  IDisposable,
  editor as MonacoEditor,
} from "monaco-editor/esm/vs/editor/editor.api";
import {
  ensureMonacoEnvironment,
  monacoThemeName,
  resolveMonacoLanguage,
  type MonacoAppearance,
  type MonacoModule,
} from "./monaco";

type MonacoCodeEditorProps = {
  appearance: MonacoAppearance;
  ariaLabel: string;
  language?: string | null;
  path?: string | null;
  readOnly?: boolean;
  value: string;
  onChange?: (value: string) => void;
  onSave?: () => void;
};

export function MonacoCodeEditor({
  appearance,
  ariaLabel,
  language,
  path,
  readOnly = false,
  value,
  onChange,
  onSave,
}: MonacoCodeEditorProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const editorRef = useRef<MonacoEditor.IStandaloneCodeEditor | null>(null);
  const modelRef = useRef<MonacoEditor.ITextModel | null>(null);
  const monacoRef = useRef<MonacoModule | null>(null);
  const modelSubscriptionRef = useRef<IDisposable | null>(null);
  const changeHandlerRef = useRef(onChange);
  const saveHandlerRef = useRef(onSave);
  const isApplyingExternalValueRef = useRef(false);
  const untitledUriRef = useRef(`inmemory://termal/${crypto.randomUUID()}`);
  const modelDescriptorRef = useRef("");

  changeHandlerRef.current = onChange;
  saveHandlerRef.current = onSave;

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }

    const monaco = ensureMonacoEnvironment();
    monacoRef.current = monaco;
    monaco.editor.setTheme(monacoThemeName(appearance));

    const editor = monaco.editor.create(container, {
      ariaLabel,
      automaticLayout: true,
      fontFamily: resolveEditorFontFamily(),
      fontSize: 13,
      lineNumbersMinChars: 4,
      minimap: { enabled: false },
      padding: { top: 14, bottom: 14 },
      readOnly,
      roundedSelection: false,
      scrollBeyondLastLine: false,
      tabSize: 2,
      wordWrap: "off",
    });
    editorRef.current = editor;

    replaceModel(value, path, language);
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
      if (!readOnly) {
        saveHandlerRef.current?.();
      }
    });

    return () => {
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

    monaco.editor.setTheme(monacoThemeName(appearance));
  }, [appearance]);

  useEffect(() => {
    editorRef.current?.updateOptions({ readOnly });
  }, [readOnly]);

  useEffect(() => {
    const nextDescriptor = describeModel(path, language);
    if (modelDescriptorRef.current !== nextDescriptor) {
      replaceModel(modelRef.current?.getValue() ?? value, path, language);
    }
  }, [language, path, value]);

  useEffect(() => {
    const model = modelRef.current;
    if (!model || model.getValue() === value) {
      return;
    }

    isApplyingExternalValueRef.current = true;
    model.setValue(value);
    isApplyingExternalValueRef.current = false;
  }, [value]);

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
      if (isApplyingExternalValueRef.current) {
        return;
      }

      changeHandlerRef.current?.(nextModel.getValue());
    });

    editor.setModel(nextModel);
    previousModel?.dispose();
    modelRef.current = nextModel;
    modelDescriptorRef.current = describeModel(nextPath, nextLanguage);
  }

  return <div ref={containerRef} className="monaco-code-editor" />;
}

function buildModelUri(monaco: MonacoModule, path: string | null | undefined, untitledUri: string) {
  const normalizedPath = path?.trim();
  return normalizedPath ? monaco.Uri.file(normalizedPath) : monaco.Uri.parse(untitledUri);
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
