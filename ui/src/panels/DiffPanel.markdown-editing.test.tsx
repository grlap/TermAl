import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { forwardRef, useEffect, useImperativeHandle, type ForwardedRef } from "react";
import mermaid from "mermaid";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchFile, fetchReviewDocument, saveReviewDocument } from "../api";
import { copyTextToClipboard } from "../clipboard";
import { DiffPanel } from "./DiffPanel";
import { hasOverlappingMarkdownCommitRanges } from "./markdown-commit-ranges";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    fetchFile: vi.fn(),
    fetchReviewDocument: vi.fn(),
    saveReviewDocument: vi.fn(),
  };
});

vi.mock("../clipboard", () => ({
  copyTextToClipboard: vi.fn(() => Promise.resolve()),
}));

vi.mock("./markdown-commit-ranges", async () => {
  const actual = await vi.importActual<typeof import("./markdown-commit-ranges")>(
    "./markdown-commit-ranges",
  );
  return {
    ...actual,
    hasOverlappingMarkdownCommitRanges: vi.fn(
      actual.hasOverlappingMarkdownCommitRanges,
    ),
  };
});

vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    render: vi.fn((id: string) =>
      Promise.resolve({
        diagramType: "flowchart",
        svg: `<svg data-testid="mermaid-svg" id="${id}"><text>diagram</text></svg>`,
      }),
    ),
  },
}));

const mermaidInitializeMock = vi.mocked(mermaid.initialize);
const mermaidRenderMock = vi.mocked(mermaid.render);

vi.mock("../MonacoDiffEditor", () => ({
  MonacoDiffEditor: forwardRef(function MonacoDiffEditorMock(
    {
      modifiedValue,
      onChange,
      onSave,
      onStatusChange,
      originalValue,
      readOnly = true,
    }: {
      modifiedValue: string;
      onChange?: (value: string) => void;
      onSave?: () => void;
      onStatusChange?: (status: {
        line: number;
        column: number;
        tabSize: number;
        insertSpaces: boolean;
        endOfLine: "LF" | "CRLF";
        changeCount: number;
        currentChange: number;
      }) => void;
      originalValue: string;
      readOnly?: boolean;
    },
    ref: ForwardedRef<{
      getScrollTop: () => number;
      goToNextChange: () => void;
      goToPreviousChange: () => void;
      setScrollTop: (scrollTop: number) => void;
    }>,
  ) {
    useImperativeHandle(ref, () => ({
      getScrollTop: () => Number((globalThis as { __termalMockDiffScrollTop?: number }).__termalMockDiffScrollTop ?? 0),
      goToNextChange: () => {},
      goToPreviousChange: () => {},
      setScrollTop: (scrollTop: number) => {
        (globalThis as { __termalMockDiffRestoredScrollTop?: number }).__termalMockDiffRestoredScrollTop = scrollTop;
      },
    }));

    useEffect(() => {
      onStatusChange?.({
        line: 1,
        column: 1,
        tabSize: 2,
        insertSpaces: true,
        endOfLine: "LF",
        changeCount: 2,
        currentChange: 1,
      });
    }, [onStatusChange]);

    return (
      <div>
        <div data-testid="monaco-diff-editor">{`${originalValue}=>${modifiedValue}`}</div>
        <textarea
          data-testid="monaco-diff-editor-modified"
          readOnly={readOnly}
          value={modifiedValue}
          onChange={(event) => onChange?.(event.target.value)}
        />
        <button type="button" onClick={() => onSave?.()}>
          Mock diff save
        </button>
      </div>
    );
  }),
}));

vi.mock("../MonacoCodeEditor", () => ({
  MonacoCodeEditor: forwardRef(function MonacoCodeEditorMock(
    {
      onChange,
      onStatusChange,
      value,
    }: {
      onChange?: (value: string) => void;
      onStatusChange?: (status: {
        line: number;
        column: number;
        tabSize: number;
        insertSpaces: boolean;
        endOfLine: "LF" | "CRLF";
      }) => void;
      value: string;
    },
    ref: ForwardedRef<{
      getScrollTop: () => number;
      setScrollTop: (scrollTop: number) => void;
    }>,
  ) {
    useImperativeHandle(ref, () => ({
      getScrollTop: () => Number((globalThis as { __termalMockCodeScrollTop?: number }).__termalMockCodeScrollTop ?? 0),
      setScrollTop: (scrollTop: number) => {
        (globalThis as { __termalMockCodeRestoredScrollTop?: number }).__termalMockCodeRestoredScrollTop = scrollTop;
      },
    }));

    useEffect(() => {
      onStatusChange?.({
        line: 1,
        column: 1,
        tabSize: 2,
        insertSpaces: true,
        endOfLine: "LF",
      });
    }, [onStatusChange]);

    return (
      <textarea
        data-testid="monaco-code-editor"
        value={value}
        onChange={(event) => onChange?.(event.target.value)}
      />
    );
  }),
}));

const fetchFileMock = vi.mocked(fetchFile);
const fetchReviewDocumentMock = vi.mocked(fetchReviewDocument);
const saveReviewDocumentMock = vi.mocked(saveReviewDocument);
const copyTextToClipboardMock = vi.mocked(copyTextToClipboard);
const hasOverlappingMarkdownCommitRangesMock = vi.mocked(
  hasOverlappingMarkdownCommitRanges,
);

async function clickAndSettle(target: HTMLElement, eventInit?: MouseEventInit) {
  await act(async () => {
    fireEvent.click(target, eventInit);
    await Promise.resolve();
  });
}

async function changeAndSettle(
  target: HTMLElement,
  eventInit: Parameters<typeof fireEvent.change>[1],
) {
  await act(async () => {
    fireEvent.change(target, eventInit);
    await Promise.resolve();
  });
}

function setCaret(target: HTMLElement, boundary: "end" | "start") {
  target.focus();
  const range = document.createRange();
  range.selectNodeContents(target);
  range.collapse(boundary === "start");
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

function setCaretInText(target: HTMLElement, text: string, offset: number) {
  const walker = document.createTreeWalker(target, NodeFilter.SHOW_TEXT);
  let currentNode = walker.nextNode();
  while (currentNode) {
    if (currentNode.textContent?.includes(text)) {
      target.focus();
      const range = document.createRange();
      range.setStart(currentNode, offset);
      range.collapse(true);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
      return currentNode;
    }
    currentNode = walker.nextNode();
  }
  throw new Error(`Unable to find rendered Markdown text node containing ${text}`);
}

function editRenderedMarkdownSection(section: HTMLElement, html: string) {
  act(() => {
    section.focus();
    const markdownRoot = section.querySelector<HTMLElement>(".markdown-copy");
    if (markdownRoot) {
      markdownRoot.innerHTML = html;
    } else {
      section.innerHTML = `<div class="markdown-copy">${html}</div>`;
    }
    fireEvent.input(section);
  });
}

function editRenderedMarkdownSectionWithoutFocus(section: HTMLElement, html: string) {
  act(() => {
    const markdownRoot = section.querySelector<HTMLElement>(".markdown-copy");
    if (markdownRoot) {
      markdownRoot.innerHTML = html;
    } else {
      section.innerHTML = `<div class="markdown-copy">${html}</div>`;
    }
    fireEvent.input(section);
  });
}

function createDeferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("DiffPanel", () => {
  beforeEach(() => {
    fetchFileMock.mockReset();
    fetchReviewDocumentMock.mockReset();
    saveReviewDocumentMock.mockReset();
    copyTextToClipboardMock.mockReset();
    copyTextToClipboardMock.mockResolvedValue(undefined);
    hasOverlappingMarkdownCommitRangesMock.mockClear();
    mermaidInitializeMock.mockClear();
    mermaidRenderMock.mockClear();
    mermaidRenderMock.mockResolvedValue({
      diagramType: "flowchart",
      svg: '<svg data-testid="mermaid-svg"><text>diagram</text></svg>',
    });
    const svgElementPrototype = SVGElement.prototype as SVGElement & {
      getBBox?: () => { height: number; width: number; x: number; y: number };
      getComputedTextLength?: () => number;
    };
    if (!svgElementPrototype.getBBox) {
      Object.defineProperty(svgElementPrototype, "getBBox", {
        configurable: true,
        value: () => ({ height: 20, width: 100, x: 0, y: 0 }),
      });
    }
    if (!svgElementPrototype.getComputedTextLength) {
      Object.defineProperty(svgElementPrototype, "getComputedTextLength", {
        configurable: true,
        value: () => 100,
      });
    }
    delete (globalThis as { __termalMockDiffRestoredScrollTop?: number }).__termalMockDiffRestoredScrollTop;
    delete (globalThis as { __termalMockDiffScrollTop?: number }).__termalMockDiffScrollTop;
    delete (globalThis as { __termalMockCodeRestoredScrollTop?: number }).__termalMockCodeRestoredScrollTop;
    delete (globalThis as { __termalMockCodeScrollTop?: number }).__termalMockCodeScrollTop;
  });

  it("edits rendered Markdown diff sections and saves the worktree file", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const savedContent = "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n";
    const onSaveFile = vi.fn().mockResolvedValue({
      content: savedContent,
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-editable"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated README"
        />,
      );
    });

    await waitFor(() => {
      expect(document.querySelectorAll(".markdown-diff-rendered-section-added").length).toBe(2);
    });

    const addedSections = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    );
    expect(addedSections[1]).toHaveTextContent("Ready to commit.");
    editRenderedMarkdownSection(addedSections[1], "<p>Ready to ship.</p>");
    expect(screen.getByRole("button", { name: "Save Markdown" })).toBeEnabled();
    fireEvent.blur(addedSections[1]);

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(onSaveFile).toHaveBeenCalledWith("/repo/README.md", savedContent, {
      baseHash: null,
      overwrite: undefined,
    });
  });

  it("commits an active rendered Markdown draft before saving from the toolbar", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const savedContent = "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n";
    const onSaveFile = vi.fn().mockResolvedValue({
      content: savedContent,
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-save-active-draft"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated README"
        />,
      );
    });

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });

    editRenderedMarkdownSection(addedSections[1], "<p>Ready to ship.</p>");
    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(onSaveFile).toHaveBeenCalledWith("/repo/README.md", savedContent, {
      baseHash: null,
      overwrite: undefined,
    });
  });

  it("does not save when a rendered Markdown draft cannot be committed", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const onSaveFile = vi.fn().mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-save-rejected-draft"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated README"
        />,
      );
    });

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });

    editRenderedMarkdownSection(addedSections[1], "<p>Ready to ship.</p>");
    hasOverlappingMarkdownCommitRangesMock.mockReturnValueOnce(true);

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(hasOverlappingMarkdownCommitRangesMock).toHaveBeenCalled();
    expect(onSaveFile).not.toHaveBeenCalled();
    expect(
      screen.getByText(
        "Save failed: Rendered Markdown edit could not be applied because the document changed under that section. Review the latest diff and edit again.",
      ),
    ).toBeInTheDocument();
    expect(addedSections[1]).toHaveTextContent("Ready to ship.");

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(onSaveFile).toHaveBeenCalledWith(
      "/repo/README.md",
      "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n",
      {
        baseHash: null,
        overwrite: undefined,
      },
    );
  });

  it("keeps the save action dirty when another rendered Markdown section reports no-op input", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-section-dirty-set"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });

    editRenderedMarkdownSection(addedSections[0], "<h1>Draft refined</h1>");
    expect(screen.getByRole("button", { name: "Save Markdown" })).toBeEnabled();

    fireEvent.input(addedSections[1]);

    expect(screen.getByRole("button", { name: "Save Markdown" })).toBeEnabled();
  });

  it("does not remount sibling rendered Markdown sections while typing", async () => {
    fetchFileMock.mockResolvedValue({
      content: [
        "# Draft document",
        "",
        "Shared intro.",
        "",
        "New one.",
        "",
        "Shared middle.",
        "",
        "New two.",
        "",
        "Shared outro.",
        "",
        "New three.",
        "",
      ].join("\n"),
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,13 +1,13 @@",
            " # Draft document",
            " Shared intro.",
            "-Old one.",
            "+New one.",
            " Shared middle.",
            "-Old two.",
            "+New two.",
            " Shared outro.",
            "-Old three.",
            "+New three.",
          ].join("\n")}
          documentContent={{
            before: {
              content: [
                "# Draft document",
                "",
                "Shared intro.",
                "",
                "Old one.",
                "",
                "Shared middle.",
                "",
                "Old two.",
                "",
                "Shared outro.",
                "",
                "Old three.",
                "",
              ].join("\n"),
              source: "index",
            },
            after: {
              content: [
                "# Draft document",
                "",
                "Shared intro.",
                "",
                "New one.",
                "",
                "Shared middle.",
                "",
                "New two.",
                "",
                "Shared outro.",
                "",
                "New three.",
                "",
              ].join("\n"),
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-section-remount"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(3);
      return sections;
    });
    const siblingOne = addedSections[1];
    const siblingTwo = addedSections[2];

    editRenderedMarkdownSection(addedSections[0], "<p>New one refined.</p>");

    const sectionsAfterEdit = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    );
    expect(sectionsAfterEdit[1]).toBe(siblingOne);
    expect(sectionsAfterEdit[2]).toBe(siblingTwo);
  });

  it("keeps rendered Markdown focus and caret when saving with Ctrl+S", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const savedContent = "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n";
    const onSaveFile = vi.fn().mockResolvedValue({
      content: savedContent,
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-ctrl-s-focus"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated README"
        />,
      );
    });

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });

    const section = addedSections[1];
    editRenderedMarkdownSection(section, "<p>Ready to ship.</p>");
    setCaretInText(section, "Ready to ship.", "Ready".length);

    await act(async () => {
      fireEvent.keyDown(section, { key: "s", ctrlKey: true });
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith("/repo/README.md", savedContent, {
        baseHash: null,
        overwrite: undefined,
      });
    });

    await waitFor(() => {
      const activeElement = document.activeElement;
      expect(activeElement).toBeInstanceOf(HTMLElement);
      expect((activeElement as HTMLElement).dataset.markdownEditable).toBe("true");
      expect(activeElement).toHaveTextContent("Ready to ship.");
      const selection = window.getSelection();
      expect(selection?.isCollapsed).toBe(true);
      expect((activeElement as HTMLElement).contains(selection?.anchorNode ?? null)).toBe(true);
      expect(selection?.anchorNode?.textContent).toContain("Ready to ship.");
      expect(selection?.anchorOffset).toBe("Ready".length);
    });
  });

  it("edits rendered Markdown sections without switching to raw text mode", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-contenteditable"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const editableSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThan(0);
      return sections;
    });
    expect(
      document.querySelector(".markdown-diff-rendered-section-removed [data-markdown-editable='true']"),
    ).toBeNull();

    expect(screen.queryByRole("textbox", { name: /Edit Markdown/ })).not.toBeInTheDocument();
    editRenderedMarkdownSection(
      editableSections[0],
      "<h1>Draft document</h1><p>literal <em>text</em></p>",
    );
    fireEvent.blur(editableSections[0]);

    expect(screen.queryByRole("textbox", { name: /Edit Markdown/ })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Draft document" })).toBeInTheDocument();
    expect(document.body).toHaveTextContent("literal text");
  });

  it("keeps pasted rendered Markdown skip subtrees in saved content", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Draft document\n\nNew section\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const onSaveFile = vi.fn().mockImplementation(async (_path: string, content: string) => ({
      content,
      language: "markdown",
      path: "/repo/README.md",
    }));

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1,3 +1,3 @@", " # Draft document", "-Old section", "+New section"].join("\n")}
          documentContent={{
            before: {
              content: "# Draft document\n\nOld section\n",
              source: "index",
            },
            after: {
              content: "# Draft document\n\nNew section\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-paste-skip-sanitize"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated README"
        />,
      );
    });

    const addedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(section).not.toBeNull();
      return section!;
    });
    const markdownRoot = addedSection.querySelector<HTMLElement>(".markdown-copy");
    expect(markdownRoot).not.toBeNull();
    setCaret(markdownRoot!, "end");
    fireEvent.paste(markdownRoot!, {
      clipboardData: {
        getData: (type: string) =>
          type === "text/html"
            ? '<div data-markdown-serialization="skip"><p>Visible pasted payload</p></div>'
            : "Visible pasted payload",
      },
    });

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(onSaveFile).toHaveBeenCalledWith(
      "/repo/README.md",
      "# Draft document\n\nNew section\n\nVisible pasted payload\n",
      {
        baseHash: null,
        overwrite: undefined,
      },
    );
  });

  it("sanitizes arbitrary rendered Markdown HTML paste before insertion", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Draft document\n\nNew section\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const onSaveFile = vi.fn().mockImplementation(async (_path: string, content: string) => ({
      content,
      language: "markdown",
      path: "/repo/README.md",
    }));

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1,3 +1,3 @@", " # Draft document", "-Old section", "+New section"].join("\n")}
          documentContent={{
            before: {
              content: "# Draft document\n\nOld section\n",
              source: "index",
            },
            after: {
              content: "# Draft document\n\nNew section\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-paste-active-sanitize"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated README"
        />,
      );
    });

    const addedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(section).not.toBeNull();
      return section!;
    });
    const markdownRoot = addedSection.querySelector<HTMLElement>(".markdown-copy");
    expect(markdownRoot).not.toBeNull();
    setCaret(markdownRoot!, "end");

    fireEvent.paste(markdownRoot!, {
      clipboardData: {
        getData: (type: string) =>
          type === "text/html"
            ? [
                '<p onclick="alert(1)" data-markdown-serialization="skip">',
                '<a href="javascript:alert(1)" onmouseover="alert(2)">Visible link</a>',
                '<svg onload="alert(3)"><text>hidden svg</text></svg>',
                '<iframe srcdoc="<script>alert(4)</script>"></iframe>',
                '<span style="color:red">safe text</span>',
                "</p>",
              ].join("")
            : "Visible link safe text",
      },
    });

    expect(markdownRoot!.querySelector("[onclick], [onmouseover], [srcdoc], [style]")).toBeNull();
    expect(markdownRoot!.querySelector("[data-markdown-serialization]")).toBeNull();
    expect(markdownRoot!.querySelector("script, svg, iframe")).toBeNull();
    expect(markdownRoot!.querySelector("a")).not.toHaveAttribute("href");
    expect(markdownRoot).toHaveTextContent("Visible linksafe text");

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(onSaveFile).toHaveBeenCalledWith(
      "/repo/README.md",
      "# Draft document\n\nNew section\n\nVisible linksafe text\n",
      {
        baseHash: null,
        overwrite: undefined,
      },
    );
  });

  // Regression: per-keystroke drafts rebuilt `segments` from a shifted
  // `editValue`, which changed positional segment IDs and unmounted the
  // focused rendered section mid-edit. Drafts stay local to the live
  // contentEditable DOM while typing, so the editor keeps focus/caret/IME
  // state until the section commits.
  it("preserves rendered section DOM identity and focus across multiple keystrokes", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-contenteditable-identity"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThan(0);
      return sections;
    });
    const editedSection = addedSections[1];
    expect(editedSection).toHaveTextContent("Ready to commit.");

    // Drive several successive input events that shift the section's line
    // count — Enter/newline insertions are the worst offender for positional
    // segment-ID churn.
    editRenderedMarkdownSection(editedSection, "<p>Ready to commit.</p><p>More details.</p>");
    const afterFirstEdit = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    )[1];
    expect(afterFirstEdit).toBe(editedSection);
    expect(document.activeElement).toBe(editedSection);
    expect(afterFirstEdit).toHaveTextContent("Ready to commit.More details.");

    editRenderedMarkdownSection(
      afterFirstEdit,
      "<p>Ready to commit.</p><p>More details.</p><p>Even more.</p>",
    );
    const afterSecondEdit = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    )[1];
    expect(afterSecondEdit).toBe(editedSection);
    expect(document.activeElement).toBe(editedSection);
    expect(afterSecondEdit).toHaveTextContent("Ready to commit.More details.Even more.");

    // Shrink the buffer back below the original line count — segment offsets
    // computed against a shifted baseline would have produced corrupted
    // content by this point on the pre-fix code path.
    editRenderedMarkdownSection(afterSecondEdit, "<p>Shipped.</p>");
    const afterThirdEdit = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    )[1];
    expect(afterThirdEdit).toBe(editedSection);
    expect(document.activeElement).toBe(editedSection);
    expect(afterThirdEdit).toHaveTextContent("Shipped.");

    fireEvent.blur(afterThirdEdit);
    expect(screen.queryByRole("textbox", { name: /Edit Markdown/ })).not.toBeInTheDocument();
    // After commit, the rendered view reflects the final draft without
    // corruption from intermediate keystrokes.
    expect(document.body.textContent).toContain("Shipped.");
    expect(document.body.textContent).not.toContain("Ready to commit.");
    expect(document.body.textContent).not.toContain("More details.");
  });

  it("cancels an uncommitted rendered Markdown section edit with Escape", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-contenteditable-escape"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const section = await waitFor(() => {
      const candidate = Array.from(
        document.querySelectorAll<HTMLElement>(
          ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
        ),
      ).find((element) => element.textContent?.includes("Ready to commit."));
      expect(candidate).not.toBeNull();
      return candidate!;
    });

    editRenderedMarkdownSection(section, "<p>Temporary draft.</p>");
    expect(screen.getByRole("button", { name: "Save Markdown" })).toBeEnabled();

    fireEvent.keyDown(section, { key: "Escape" });

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Saved" })).toBeDisabled();
      expect(section).toHaveTextContent("Ready to commit.");
    });
    expect(document.body).not.toHaveTextContent("Temporary draft.");
  });

  // Regression: clicking anywhere inside a rendered Markdown section used to
  // enter edit mode on the same mouseup, collapsing any drag-selection the
  // user had just made. The click handler now skips `startEditing` when the
  // window has a non-collapsed selection.
  it("does not enter edit mode when the click completes a text selection", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-click-selection"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const normalSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>(
        ".markdown-diff-normal-section[data-markdown-editable='true']",
      );
      expect(section).not.toBeNull();
      return section!;
    });
    const targetNode = normalSection.querySelector("p") ?? normalSection;
    const range = document.createRange();
    range.selectNodeContents(targetNode);
    const selection = window.getSelection();
    expect(selection).not.toBeNull();
    selection!.removeAllRanges();
    selection!.addRange(range);
    expect(selection!.isCollapsed).toBe(false);

    fireEvent.click(targetNode);

    expect(
      screen.queryByRole("textbox", { name: "Edit Markdown normal section" }),
    ).not.toBeInTheDocument();
    // A fresh selection remains unaffected and can still be observed.
    const stillSelected = window.getSelection();
    expect(stillSelected?.isCollapsed).toBe(false);
  });

  // Regression: editable sections previously had no text-editing semantics
  // (and an earlier variant used role=button on a rich editor subtree).
  it("marks editable Markdown sections as multiline textboxes, not buttons", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-aria-role"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const editableSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        "[data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThan(0);
      return sections;
    });
    for (const section of Array.from(editableSections)) {
      expect(section).toHaveAttribute("role", "textbox");
      expect(section).toHaveAttribute("aria-multiline", "true");
      expect(section).not.toHaveAttribute("role", "button");
    }
    expect(
      screen.getAllByRole("textbox", {
        name: /Edit (added|unchanged) Markdown section/,
      }).length,
    ).toBe(editableSections.length);
  });

  it("moves the rendered Markdown caret between editable sections and skips deleted sections", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-caret"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const editableSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>("[data-markdown-editable='true']");
      expect(sections.length).toBeGreaterThan(2);
      return sections;
    });
    const deletedSections = document.querySelectorAll(".markdown-diff-rendered-section-removed");
    expect(deletedSections.length).toBeGreaterThan(0);

    setCaret(editableSections[0], "end");
    fireEvent.keyDown(editableSections[0], { key: "ArrowDown" });

    expect(document.activeElement).toBe(editableSections[1]);
    expect(editableSections[1].textContent).toContain("Draft document");
    expect(window.getSelection()?.anchorNode?.nodeType).toBe(Node.TEXT_NODE);
    expect(editableSections[1].contains(window.getSelection()?.anchorNode ?? null)).toBe(true);

    setCaret(editableSections[1], "start");
    fireEvent.keyDown(editableSections[1], { key: "ArrowUp" });

    expect(document.activeElement).toBe(editableSections[0]);
  });

  it("lets ArrowUp and ArrowDown stay inside rendered Markdown sections until the text boundary", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-caret-native"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const editableSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>("[data-markdown-editable='true']");
      expect(sections.length).toBeGreaterThan(2);
      return sections;
    });

    const textNode = setCaretInText(editableSections[1], "Draft document", 3);
    fireEvent.keyDown(editableSections[1], { key: "ArrowDown" });

    expect(document.activeElement).toBe(editableSections[1]);
    expect(window.getSelection()?.anchorNode).toBe(textNode);

    fireEvent.keyDown(editableSections[1], { key: "ArrowUp" });

    expect(document.activeElement).toBe(editableSections[1]);
    expect(window.getSelection()?.anchorNode).toBe(textNode);
  });

  it("moves the rendered Markdown caret with PageUp and PageDown", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-page-caret"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const editableSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>("[data-markdown-editable='true']");
      expect(sections.length).toBeGreaterThan(2);
      return sections;
    });

    setCaretInText(editableSections[1], "Draft document", 3);
    fireEvent.keyDown(editableSections[1], { key: "PageDown" });

    expect(document.activeElement).toBe(editableSections[2]);
    expect(window.getSelection()?.anchorNode?.nodeType).toBe(Node.TEXT_NODE);
    expect(editableSections[2].contains(window.getSelection()?.anchorNode ?? null)).toBe(true);

    fireEvent.keyDown(editableSections[2], { key: "PageUp" });

    expect(document.activeElement).toBe(editableSections[1]);
    expect(window.getSelection()?.anchorNode?.nodeType).toBe(Node.TEXT_NODE);
    expect(editableSections[1].contains(window.getSelection()?.anchorNode ?? null)).toBe(true);
  });

  it("keeps a visible caret when leaving a dirty rendered Markdown section", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " Shared intro.",
            "-# Base document",
            "+# Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "index",
            },
            after: {
              content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-dirty-caret-crossing"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });
    const section = Array.from(addedSections).find((candidate) =>
      candidate.textContent?.includes("Ready to commit."),
    );
    expect(section).toBeTruthy();

    editRenderedMarkdownSection(section!, "<p>Ready to ship.</p>");
    setCaret(section!, "end");
    fireEvent.keyDown(section!, { key: "ArrowDown" });

    const activeElement = document.activeElement;
    expect(activeElement).toBeInstanceOf(HTMLElement);
    expect((activeElement as HTMLElement).dataset.markdownEditable).toBe("true");
    expect(activeElement).toHaveTextContent("Shared outro.");
    expect(window.getSelection()?.anchorNode?.nodeType).toBe(Node.TEXT_NODE);
    expect((activeElement as HTMLElement).contains(window.getSelection()?.anchorNode ?? null)).toBe(true);
  });

  it("does not rewrite rendered Markdown sections when only moving the caret", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Title\n\n* First item\n* Second item\n\nTail updated.\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const onSaveFile = vi.fn().mockResolvedValue(undefined);

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,7 +1,7 @@",
            " # Title",
            " ",
            "-Old list.",
            "+* First item",
            "+* Second item",
            " ",
            "-Old tail.",
            "+Tail updated.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Title\n\nOld list.\n\nOld tail.\n",
              source: "index",
            },
            after: {
              content: "# Title\n\n* First item\n* Second item\n\nTail updated.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-caret-only-navigation"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated README"
        />,
      );
    });

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });
    const listSection = Array.from(addedSections).find((section) =>
      section.textContent?.includes("First item"),
    );
    expect(listSection).not.toBeNull();
    const editableListSection = listSection as HTMLElement;
    expect(screen.getByRole("button", { name: "Saved" })).toBeDisabled();

    for (const eventInit of [
      { key: "ArrowDown", boundary: "end" as const },
      { key: "PageDown", boundary: "end" as const },
      { key: "PageUp", boundary: "start" as const },
      { key: "s", ctrlKey: true, boundary: "end" as const },
    ]) {
      setCaret(editableListSection, eventInit.boundary);
      fireEvent.keyDown(editableListSection, eventInit);
      fireEvent.blur(editableListSection);

      expect(screen.getByRole("button", { name: "Saved" })).toBeDisabled();
      expect(screen.queryByRole("button", { name: "Save Markdown" })).toBeNull();
      expect(document.querySelectorAll(".markdown-diff-rendered-section-added")).toHaveLength(2);
    }

    expect(onSaveFile).not.toHaveBeenCalled();
  });

  it("preserves an uncommitted downstream rendered draft when another section shifts line counts", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Title\n\nSection one original.\n\nSection two original.\n",
      language: "markdown",
      path: "/repo/notes.md",
    });
    const savedCapture: Array<{ path: string; content: string }> = [];
    const onSaveFile = vi
      .fn()
      .mockImplementation(async (path: string, content: string) => {
        savedCapture.push({ path, content });
        return { content, language: "markdown", path };
      });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " # Title",
            " ",
            "-Section one base.",
            "+Section one original.",
            " ",
            "-Section two base.",
            "+Section two original.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Title\n\nSection one base.\n\nSection two base.\n",
              source: "index",
            },
            after: {
              content: "# Title\n\nSection one original.\n\nSection two original.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-line-shift-draft"
          filePath="/repo/notes.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated notes"
        />,
      );
    });

    const sections = await waitFor(() => {
      const added = Array.from(
        document.querySelectorAll<HTMLElement>(
          ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
        ),
      );
      expect(added.length).toBeGreaterThanOrEqual(2);
      return added;
    });
    const sectionOne = sections.find((section) =>
      section.textContent?.includes("Section one original."),
    );
    const sectionTwo = sections.find((section) =>
      section.textContent?.includes("Section two original."),
    );
    expect(sectionOne).toBeTruthy();
    expect(sectionTwo).toBeTruthy();

    editRenderedMarkdownSection(sectionTwo!, "<p>Section two in progress.</p>");
    expect(document.activeElement).toBe(sectionTwo);

    editRenderedMarkdownSectionWithoutFocus(
      sectionOne!,
      "<p>Section one revised.</p><p>Extra line shifts offsets.</p>",
    );
    fireEvent.blur(sectionOne!);

    const sectionTwoAfterShift = await waitFor(() => {
      const candidate = Array.from(
        document.querySelectorAll<HTMLElement>("[data-markdown-editable='true']"),
      ).find((section) => section.textContent?.includes("Section two in progress."));
      expect(candidate).toBeTruthy();
      return candidate!;
    });

    fireEvent.blur(sectionTwoAfterShift);
    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(savedCapture.length).toBeGreaterThan(0);
    expect(savedCapture[savedCapture.length - 1].content).toBe(
      "# Title\n\nSection one revised.\n\nExtra line shifts offsets.\n\nSection two in progress.\n",
    );
  });

  // Regression: the previous freeze pattern captured a single
  // `frozenSegmentSourceContentRef` that was only thawed when the
  // `activeEditingCount` read 0 during render. When the user committed
  // section A then immediately started editing section B, both state
  // updates (A's commit flushing + B's start incrementing) could flush
  // through React without the counter ever hitting 0 in a render, so
  // B's edit was applied against the pre-A-commit baseline and A's
  // changes were silently overwritten. The fix is to capture the source
  // content fresh at each section's edit-start so the next edit always
  // applies to the post-previous-commit baseline.
  it("preserves prior-section edits when the user commits section A then edits section B", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Title\n\nSection one original.\n\nSection two original.\n",
      language: "markdown",
      path: "/repo/notes.md",
    });
    const savedCapture: Array<{ path: string; content: string }> = [];
    const onSaveFile = vi
      .fn()
      .mockImplementation(async (path: string, content: string) => {
        savedCapture.push({ path, content });
        return { content, language: "markdown", path };
      });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " # Title",
            " ",
            "-Section one base.",
            "+Section one original.",
            " ",
            "-Section two base.",
            "+Section two original.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Title\n\nSection one base.\n\nSection two base.\n",
              source: "index",
            },
            after: {
              content: "# Title\n\nSection one original.\n\nSection two original.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-multi-section-commit"
          filePath="/repo/notes.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated notes"
        />,
      );
    });

    const addedSectionsInitial = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      // Two distinct added sections: one for each line that changed.
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });
    const sectionOneA = Array.from(addedSectionsInitial).find((section) =>
      section.textContent?.includes("Section one original."),
    );
    expect(sectionOneA).toBeTruthy();

    // Begin editing section A.
    editRenderedMarkdownSection(sectionOneA!, "<p>Section one revised.</p>");

    // Immediately click into section B WITHOUT first blurring A. In a
    // real click, the browser fires blur → mouseup → click in one
    // gesture, so A commits and B starts editing within the same tick.
    // We simulate the same sequence with fireEvent.
    const addedSectionsAfterA = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      return sections;
    });
    const sectionTwoA = Array.from(addedSectionsAfterA).find((section) =>
      section.textContent?.includes("Section two original."),
    );
    expect(sectionTwoA).toBeTruthy();
    // Real browsers dispatch blur on A *and* the click on B as a single
    // user interaction. Wrap both in one `act` so React batches the state
    // updates together — this is what exercises the freeze/thaw transition
    // that the counter-based implementation silently skips.
    await act(async () => {
      fireEvent.blur(sectionOneA!);
      fireEvent.click(sectionTwoA!);
      await Promise.resolve();
    });

    const addedSectionsAfterCommit = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });
    const sectionTwoB = Array.from(addedSectionsAfterCommit).find((section) =>
      section.textContent?.includes("Section two original."),
    );
    expect(sectionTwoB).toBeTruthy();
    editRenderedMarkdownSection(sectionTwoB!, "<p>Section two refined.</p>");
    fireEvent.blur(sectionTwoB!);

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    // The saved content must contain BOTH edits. Before the fix, B's
    // commit applied to a stale baseline that still had "Section one
    // original.", so A's edit was silently dropped.
    expect(savedCapture.length).toBeGreaterThan(0);
    const latestSave = savedCapture[savedCapture.length - 1];
    expect(latestSave.path).toBe("/repo/notes.md");
    expect(latestSave.content).toBe(
      "# Title\n\nSection one revised.\n\nSection two refined.\n",
    );
  });

  // Regression: the frozen segment-source baseline used to stay pinned to
  // the content the user was editing when a watcher-driven rebase updated
  // `editValue` mid-edit. The next rendered commit then replayed the section
  // edit against the stale baseline and silently dropped the rebased
  // on-disk changes. The current implementation does not propagate drafts
  // to `editValue` at all, so the watcher rebase path uses the committed
  // content and the next commit reads the live post-rebase baseline.
  it("preserves code-mode edits when committing a rendered Markdown section edit", async () => {
    // Use a fixture with an unchanged paragraph between each change so the
    // diff produces distinct one-line segments rather than grouping
    // consecutive changes into a single multi-line block.
    fetchFileMock.mockResolvedValue({
      content: "# Title\n\nSection one original.\n\nSection two original.\n",
      language: "markdown",
      path: "/repo/notes.md",
    });
    const savedCapture: Array<{ path: string; content: string }> = [];
    const onSaveFile = vi
      .fn()
      .mockImplementation(async (path: string, content: string) => {
        savedCapture.push({ path, content });
        return { content, language: "markdown", path };
      });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            " # Title",
            " ",
            "-Section one base.",
            "+Section one original.",
            " ",
            " Section two original.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Title\n\nSection one base.\n\nSection two original.\n",
              source: "index",
            },
            after: {
              content: "# Title\n\nSection one original.\n\nSection two original.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-code-mode-carry"
          filePath="/repo/notes.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated notes"
        />,
      );
    });

    // Drift editValue via the code-mode Monaco mock: change the
    // previously-unchanged "Section two original." to "Section two refined."
    // while leaving the other unchanged text alone. This is the exact
    // scenario the Codex review flagged as High.
    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));
    await changeAndSettle(await screen.findByTestId("monaco-code-editor"), {
      target: {
        value: "# Title\n\nSection one original.\n\nSection two refined.\n",
      },
    });
    // Switch back to rendered Markdown — the display preview now uses the
    // dirty editValue, so rendered segments should reflect the drift.
    await clickAndSettle(screen.getByRole("button", { name: "Rendered Markdown" }));

    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThan(0);
      return sections;
    });
    // Edit the "Section one original." line in rendered mode.
    const sectionOne = Array.from(addedSections).find((section) =>
      section.textContent?.includes("Section one original."),
    );
    expect(sectionOne).toBeTruthy();
    editRenderedMarkdownSection(sectionOne!, "<p>Section one revised.</p>");
    fireEvent.blur(sectionOne!);

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    // The saved content must carry BOTH the code-mode edit
    // ("Section two refined.") AND the rendered-mode edit
    // ("Section one revised.") without corrupting the surrounding lines.
    expect(savedCapture.length).toBeGreaterThan(0);
    const latestSave = savedCapture[savedCapture.length - 1];
    expect(latestSave.path).toBe("/repo/notes.md");
    expect(latestSave.content).toBe(
      "# Title\n\nSection one revised.\n\nSection two refined.\n",
    );
  });


});
