import { useEffect, useLayoutEffect, useRef } from "react";
import type { editor as MonacoEditor } from "monaco-editor/esm/vs/editor/editor.api";
import {
  ensureMonacoEnvironment,
  monacoThemeName,
  resolveMonacoLanguage,
  type MonacoAppearance,
  type MonacoModule,
} from "./monaco";

type MonacoDiffEditorProps = {
  appearance: MonacoAppearance;
  ariaLabel: string;
  language?: string | null;
  path?: string | null;
  modifiedValue: string;
  originalValue: string;
};

export function MonacoDiffEditor({
  appearance,
  ariaLabel,
  language,
  path,
  modifiedValue,
  originalValue,
}: MonacoDiffEditorProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const diffEditorRef = useRef<MonacoEditor.IStandaloneDiffEditor | null>(null);
  const monacoRef = useRef<MonacoModule | null>(null);
  const originalModelRef = useRef<MonacoEditor.ITextModel | null>(null);
  const modifiedModelRef = useRef<MonacoEditor.ITextModel | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const untitledUriRef = useRef(`inmemory://termal-diff/${crypto.randomUUID()}`);
  const modelDescriptorRef = useRef("");

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }

    const monaco = ensureMonacoEnvironment();
    monacoRef.current = monaco;
    monaco.editor.setTheme(monacoThemeName(appearance));

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
      roundedSelection: false,
      scrollBeyondLastLine: false,
      stickyScroll: { enabled: false },
      useInlineViewWhenSpaceIsLimited: false,
      wordWrap: "off",
    });
    diffEditorRef.current = editor;

    resizeObserverRef.current = new ResizeObserver(() => {
      window.requestAnimationFrame(layoutEditor);
    });
    resizeObserverRef.current.observe(container);
    if (container.parentElement) {
      resizeObserverRef.current.observe(container.parentElement);
    }

    window.requestAnimationFrame(() => {
      layoutEditor();
      window.requestAnimationFrame(layoutEditor);
    });

    replaceModels(originalValue, modifiedValue, path, language);

    return () => {
      resizeObserverRef.current?.disconnect();
      resizeObserverRef.current = null;
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

    monaco.editor.setTheme(monacoThemeName(appearance));
    layoutEditor();
  }, [appearance]);

  useEffect(() => {
    const nextDescriptor = describeModel(path, language);
    if (modelDescriptorRef.current !== nextDescriptor) {
      replaceModels(originalValue, modifiedValue, path, language);
    }
    layoutEditor();
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
  }, [modifiedValue, originalValue]);

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
  }

  return <div ref={containerRef} className="monaco-diff-editor" />;
}

function buildModelUri(
  monaco: MonacoModule,
  baseUri: string,
  path: string | null | undefined,
  role: "original" | "modified",
) {
  const suffix = path?.trim().split(/[/\\]+/).filter(Boolean).join("-") ?? "untitled";
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
