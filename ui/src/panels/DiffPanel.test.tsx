import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { forwardRef, useEffect, useImperativeHandle, type ForwardedRef } from "react";
import mermaid from "mermaid";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchFile, fetchReviewDocument, saveReviewDocument } from "../api";
import { copyTextToClipboard } from "../clipboard";
import { DiffPanel } from "./DiffPanel";

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

  it("defaults to the full diff view and supports changed-only and edit modes", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const latest = true;\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,2 +1,3 @@",
            "-const before = false;",
            "+const after = true;",
            " unchanged",
            "+const latest = true;",
          ].join("\n")}
          diffMessageId="diff-1"
          filePath="/repo/src/example.ts"
          gitSectionId="staged"
          language="typescript"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      );
    });

    expect(screen.getByLabelText("Changed lines: 1")).toHaveTextContent("1");
    expect(screen.getByLabelText("Added lines: 1")).toHaveTextContent("+1");
    expect(screen.getByText("Staged")).toBeInTheDocument();
    expect(screen.queryByText("File edit")).not.toBeInTheDocument();
    expect(screen.queryByText("Updated example file")).not.toBeInTheDocument();
    expect(screen.getByText("src/example.ts")).not.toHaveClass("chip");
    expect(document.querySelector('.diff-preview-file-icon[data-file-kind="typescript"]')).not.toBeNull();
    await clickAndSettle(screen.getByRole("button", { name: "Copy path" }));
    expect(copyTextToClipboardMock).toHaveBeenCalledWith("src/example.ts");
    expect(await screen.findByTestId("monaco-diff-editor")).toBeInTheDocument();
    expect(screen.getByText("Change 1 of 2")).toBeInTheDocument();

    await clickAndSettle(screen.getByRole("button", { name: "Changed only" }));
    expect(await screen.findByTestId("structured-diff-view")).toBeInTheDocument();
    expect(screen.getByText("@@ -1,2 +1,3 @@")).toBeInTheDocument();

    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledWith("/repo/src/example.ts", { sessionId: "session-1", projectId: null });
    });

    expect(await screen.findByTestId("monaco-code-editor")).toHaveValue("const latest = true;\n");
  });

  it("does not show rendered Markdown mode for non-Markdown diffs", async () => {
    fetchFileMock.mockResolvedValue({
      content: "export const latest = true;\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1 +1 @@", "-export const old = false;", "+export const latest = true;"].join("\n")}
          diffMessageId="diff-no-markdown-mode"
          filePath="/repo/src/example.ts"
          gitSectionId="unstaged"
          language="typescript"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      );
    });

    expect(screen.queryByRole("button", { name: "Rendered Markdown" })).toBeNull();
  });

  it("renders staged Markdown from the index document side instead of the worktree file", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Worktree document\n\nThis is not staged.\n",
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
            "+# Staged document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "head",
            },
            after: {
              content: "Shared intro.\n# Staged document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "index",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-staged"
          filePath="/repo/README.md"
          gitSectionId="staged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    await waitFor(() => {
      expect(document.querySelector(".markdown-diff-rendered-section-added")).not.toBeNull();
      expect(document.querySelector(".markdown-diff-rendered-section-removed")).not.toBeNull();
    });
    expect(screen.queryByText("Added")).not.toBeInTheDocument();
    expect(screen.queryByText("Deleted")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Staged document" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Base document" })).toBeInTheDocument();
    expect(screen.getByText("Shared intro.")).toBeInTheDocument();
    expect(screen.getByText("Shared middle.")).toBeInTheDocument();
    expect(screen.getByText("Shared outro.")).toBeInTheDocument();
    expect(screen.getByText("Ready to commit.")).toBeInTheDocument();
    expect(
      document.querySelector(".markdown-diff-normal-section [data-markdown-line-start='1']"),
    ).not.toBeNull();
    expect(
      document.querySelector(".markdown-diff-rendered-section-added [data-markdown-line-start='2']"),
    ).not.toBeNull();
    expect(
      document.querySelector(".markdown-diff-rendered-section-removed [data-markdown-line-start='2']"),
    ).not.toBeNull();
    expect(
      document.querySelector(".markdown-diff-rendered-section-added [data-markdown-line-start='4']"),
    ).not.toBeNull();
    expect(
      document.querySelector(".markdown-diff-rendered-section-removed [data-markdown-line-start='4']"),
    ).not.toBeNull();
    expect(screen.queryByRole("heading", { name: "Worktree document" })).not.toBeInTheDocument();
    expect(screen.getAllByText("Index").length).toBeGreaterThan(0);
    expect(screen.queryByRole("button", { name: "After" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Before" })).not.toBeInTheDocument();
  });

  it("preserves scroll offsets when switching between file and rendered Markdown diff views", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Worktree document\n\nThis is not staged.\n",
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
            "@@ -1,6 +1,6 @@",
            " # Document",
            " Intro",
            "-Old section",
            "+New section",
            " Middle",
            " Tail",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Document\nIntro\nOld section\nMiddle\nTail\n",
              source: "head",
            },
            after: {
              content: "# Document\nIntro\nNew section\nMiddle\nTail\n",
              source: "index",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-scroll-memory"
          filePath="/repo/README.md"
          gitSectionId="staged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    const markdownScroll = await waitFor(() => {
      const scrollRegion = document.querySelector<HTMLElement>(".markdown-diff-change-scroll");
      expect(scrollRegion).not.toBeNull();
      return scrollRegion!;
    });
    markdownScroll.scrollTop = 420;

    await clickAndSettle(screen.getByRole("button", { name: "All lines" }));
    expect(await screen.findByTestId("monaco-diff-editor")).toBeInTheDocument();
    (globalThis as { __termalMockDiffScrollTop?: number }).__termalMockDiffScrollTop = 180;

    await clickAndSettle(screen.getByRole("button", { name: "Rendered Markdown" }));

    await waitFor(() => {
      expect(document.querySelector<HTMLElement>(".markdown-diff-change-scroll")?.scrollTop).toBe(420);
    });

    await clickAndSettle(screen.getByRole("button", { name: "All lines" }));

    await waitFor(() => {
      expect((globalThis as { __termalMockDiffRestoredScrollTop?: number }).__termalMockDiffRestoredScrollTop).toBe(180);
    });
  });

  it("preserves scroll offsets for changed-only, raw, and edit diff views", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Worktree document\n\nNew section\nMiddle\nTail\n",
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
            "@@ -1,6 +1,6 @@",
            " # Document",
            " Intro",
            "-Old section",
            "+New section",
            " Middle",
            " Tail",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Document\nIntro\nOld section\nMiddle\nTail\n",
              source: "head",
            },
            after: {
              content: "# Document\nIntro\nNew section\nMiddle\nTail\n",
              source: "index",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-scroll-memory-non-default"
          filePath="/repo/README.md"
          gitSectionId="staged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated README"
        />,
      );
    });

    await clickAndSettle(screen.getByRole("button", { name: "Changed only" }));
    const structuredScroll = await screen.findByTestId("structured-diff-view");
    structuredScroll.scrollTop = 321;

    await clickAndSettle(screen.getByRole("button", { name: "Raw patch" }));
    const rawScroll = document.querySelector<HTMLElement>(".diff-preview-raw-shell");
    expect(rawScroll).not.toBeNull();
    rawScroll!.scrollTop = 654;

    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));
    expect(await screen.findByTestId("monaco-code-editor")).toBeInTheDocument();
    (globalThis as { __termalMockCodeScrollTop?: number }).__termalMockCodeScrollTop = 222;

    await clickAndSettle(screen.getByRole("button", { name: "Rendered Markdown" }));

    await clickAndSettle(screen.getByRole("button", { name: "Changed only" }));
    await waitFor(() => {
      expect(screen.getByTestId("structured-diff-view").scrollTop).toBe(321);
    });

    await clickAndSettle(screen.getByRole("button", { name: "Raw patch" }));
    await waitFor(() => {
      expect(document.querySelector<HTMLElement>(".diff-preview-raw-shell")?.scrollTop).toBe(654);
    });

    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));
    await waitFor(() => {
      expect((globalThis as { __termalMockCodeRestoredScrollTop?: number }).__termalMockCodeRestoredScrollTop).toBe(222);
    });
  });

  it("resets rendered Markdown scroll when switching between same-mode diff tabs", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Worktree document\n\nThis is not staged.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    const firstDocumentContent = {
      before: {
        content: "# First\n\nOld section\n",
        source: "head" as const,
      },
      after: {
        content: "# First\n\nNew section\n",
        source: "index" as const,
      },
      canEdit: true,
      isCompleteDocument: true,
    };
    const secondDocumentContent = {
      before: {
        content: "# Second\n\nOld section\n",
        source: "head" as const,
      },
      after: {
        content: "# Second\n\nNew section\n",
        source: "index" as const,
      },
      canEdit: true,
      isCompleteDocument: true,
    };

    const { rerender } = render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,3 +1,3 @@", " # First", "-Old section", "+New section"].join("\n")}
        documentContent={firstDocumentContent}
        diffMessageId="diff-markdown-scroll-first"
        filePath="/repo/FIRST.md"
        gitSectionId="staged"
        language="markdown"
        sessionId="session-1"
        workspaceRoot="/repo"
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated first doc"
      />,
    );

    const firstScrollRegion = await waitFor(() => {
      const scrollRegion = document.querySelector<HTMLElement>(".markdown-diff-change-scroll");
      expect(scrollRegion).not.toBeNull();
      return scrollRegion!;
    });
    firstScrollRegion.scrollTop = 420;

    rerender(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,3 +1,3 @@", " # Second", "-Old section", "+New section"].join("\n")}
        documentContent={secondDocumentContent}
        diffMessageId="diff-markdown-scroll-second"
        filePath="/repo/SECOND.md"
        gitSectionId="staged"
        language="markdown"
        sessionId="session-1"
        workspaceRoot="/repo"
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated second doc"
      />,
    );

    await waitFor(() => {
      expect(document.querySelector<HTMLElement>(".markdown-diff-change-scroll")?.scrollTop).toBe(0);
    });
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

  it("does not re-register every rendered Markdown committer while typing", async () => {
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

    const originalAdd = Set.prototype.add;
    const originalDelete = Set.prototype.delete;
    let registryAdds = 0;
    let registryDeletes = 0;
    const isRenderedMarkdownCommitter = (value: unknown) =>
      typeof value === "function" && value.toString().includes("collectSectionEdit");
    const addSpy = vi.spyOn(Set.prototype, "add").mockImplementation(function add(
      this: Set<unknown>,
      value: unknown,
    ) {
      if (isRenderedMarkdownCommitter(value)) {
        registryAdds += 1;
      }
      return originalAdd.call(this, value);
    });
    const deleteSpy = vi.spyOn(Set.prototype, "delete").mockImplementation(function deleteValue(
      this: Set<unknown>,
      value: unknown,
    ) {
      if (isRenderedMarkdownCommitter(value)) {
        registryDeletes += 1;
      }
      return originalDelete.call(this, value);
    });

    try {
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
            diffMessageId="diff-markdown-committer-registry"
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

      registryAdds = 0;
      registryDeletes = 0;
      editRenderedMarkdownSection(addedSections[0], "<p>New one refined.</p>");

      expect(registryAdds).toBe(0);
      expect(registryDeletes).toBe(0);
    } finally {
      addSpy.mockRestore();
      deleteSpy.mockRestore();
    }
  });

  it("keeps rendered Markdown edits typed while save is pending", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const firstSave = createDeferred<{
      content: string;
      language: string;
      path: string;
    }>();
    const savedCapture: Array<{ content: string; path: string }> = [];
    const onSaveFile = vi
      .fn()
      .mockImplementationOnce(async (path: string, content: string) => {
        savedCapture.push({ content, path });
        return firstSave.promise;
      })
      .mockImplementation(async (path: string, content: string) => {
        savedCapture.push({ content, path });
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
          diffMessageId="diff-markdown-save-pending-draft"
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
    const targetSection = addedSections[1];
    const firstContent = "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n";
    const secondContent = "Shared intro.\n# Draft document\nShared middle.\nReady to launch.\nShared outro.\n";

    editRenderedMarkdownSection(targetSection, "<p>Ready to ship.</p>");
    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(savedCapture[0]).toEqual({ content: firstContent, path: "/repo/README.md" });
    const pendingSection = await waitFor(() => {
      const section = Array.from(
        document.querySelectorAll<HTMLElement>(
          ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
        ),
      ).find((candidate) => candidate.textContent?.includes("Ready to ship."));
      expect(section).toBeTruthy();
      return section!;
    });
    editRenderedMarkdownSection(pendingSection, "<p>Ready to launch.</p>");

    await act(async () => {
      firstSave.resolve({
        content: firstContent,
        language: "markdown",
        path: "/repo/README.md",
      });
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Save Markdown" })).toBeEnabled();
    });
    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(savedCapture[savedCapture.length - 1]).toEqual({
      content: secondContent,
      path: "/repo/README.md",
    });
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

  it("renders Mermaid diagrams while keeping blocks as editable source in rendered Markdown diffs", async () => {
    fetchFileMock.mockResolvedValue({
      content: ["# Diagram", "", "```mermaid", "flowchart TD", "  A --> B", "```", ""].join("\n"),
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
            "@@ -1,2 +1,6 @@",
            " # Diagram",
            " ",
            "+```mermaid",
            "+flowchart TD",
            "+  A --> B",
            "+```",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Diagram\n\n",
              source: "index",
            },
            after: {
              content: ["# Diagram", "", "```mermaid", "flowchart TD", "  A --> B", "```", ""].join("\n"),
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-mermaid-source"
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

    await waitFor(() => {
      expect(
        document.querySelector(
          ".markdown-diff-rendered-section-added [data-markdown-editable='true'] code.language-mermaid",
        )?.textContent,
      ).toBe("flowchart TD\n  A --> B");
    });
    await waitFor(() => {
      expect(
        screen.queryByTestId("mermaid-svg") ??
          document.querySelector(".markdown-diff-rendered-section-added .mermaid-diagram-svg svg"),
      ).toBeInTheDocument();
    });
  });

  it("keeps rendered staged Markdown read-only while preserving caret navigation", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Staged document\nShared middle.\nReady to commit.\nShared outro.\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const onSaveFile = vi.fn();

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
            "+# Staged document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "head",
            },
            after: {
              content: "Shared intro.\n# Staged document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "index",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-staged-editable"
          filePath="/repo/README.md"
          gitSectionId="staged"
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
      ".markdown-diff-rendered-section-added [data-markdown-caret='true']",
    );
    const removedSections = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-removed [data-markdown-caret='true']",
    );
    expect(removedSections).toHaveLength(0);
    expect(addedSections[1]).toHaveTextContent("Ready to commit.");
    expect(addedSections[1]).toHaveAttribute("contenteditable", "true");
    expect(addedSections[1]).toHaveAttribute("aria-readonly", "true");
    expect(addedSections[1]).toHaveAttribute("data-markdown-readonly", "true");
    expect(addedSections[1]).not.toHaveAttribute("data-markdown-editable");
    expect(
      screen.getAllByText("Staged Markdown diffs are read-only. Use the unstaged worktree diff to edit this file.")
        .length,
    ).toBeGreaterThan(0);
    expect(screen.queryByRole("button", { name: "Save Markdown" })).not.toBeInTheDocument();

    const removedBody = document.querySelector<HTMLElement>(
      ".markdown-diff-rendered-section-removed .markdown-diff-rendered-section-body",
    );
    const removedTextNode = document.createTreeWalker(removedBody!, NodeFilter.SHOW_TEXT).nextNode();
    const range = document.createRange();
    range.setStart(removedTextNode!, 0);
    range.collapse(true);
    window.getSelection()?.removeAllRanges();
    window.getSelection()?.addRange(range);
    fireEvent.keyDown(document.querySelector<HTMLElement>(".markdown-diff-change-scroll")!, { key: "ArrowDown" });
    expect(addedSections[0].contains(window.getSelection()?.anchorNode ?? null)).toBe(true);

    addedSections[1].innerHTML = "<p>Ready to save.</p>";
    fireEvent.input(addedSections[1]);

    await waitFor(() => {
      expect(addedSections[1]).toHaveTextContent("Ready to commit.");
    });
    expect(onSaveFile).not.toHaveBeenCalled();

    await clickAndSettle(screen.getByRole("button", { name: "All lines" }));
    const editor = await screen.findByTestId("monaco-diff-editor-modified");
    expect(editor).toHaveAttribute("readonly");
  });

  it("keeps rendered staged Markdown read-only when the worktree has unstaged changes", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Worktree content that is not staged.\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const onSaveFile = vi.fn();

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
            "+# Staged document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared middle.\nCommitted text.\nShared outro.\n",
              source: "head",
            },
            after: {
              content: "Shared intro.\n# Staged document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "index",
            },
            canEdit: false,
            editBlockedReason:
              "This staged Markdown diff is read-only because the worktree has unstaged changes for this file.",
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-staged-readonly"
          filePath="/repo/README.md"
          gitSectionId="staged"
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
      expect(screen.getByRole("heading", { name: "Staged document" })).toBeInTheDocument();
    });

    expect(
      screen.getAllByText("This staged Markdown diff is read-only because the worktree has unstaged changes for this file.")
        .length,
    ).toBeGreaterThan(0);
    expect(document.querySelector("[data-markdown-editable='true']")).toBeNull();
    expect(screen.queryByRole("button", { name: "Save Markdown" })).not.toBeInTheDocument();
    expect(onSaveFile).not.toHaveBeenCalled();
  });

  it("keeps Monaco inline editing enabled for Markdown diffs in all-lines view", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Draft document\n\nReady to commit.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1,3 +1,3 @@", "-# Base document", "+# Draft document", " Ready to commit."].join("\n")}
          documentContent={{
            before: {
              content: "# Base document\n\nReady to commit.\n",
              source: "index",
            },
            after: {
              content: "# Draft document\n\nReady to commit.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-all-edit"
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

    await clickAndSettle(screen.getByRole("button", { name: "All lines" }));

    const editor = await screen.findByTestId("monaco-diff-editor-modified");
    expect(editor).not.toHaveAttribute("readonly");
  });

  it("keeps the selected Markdown view mode when document content availability changes", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Draft document\n\nReady to commit.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    const baseProps = {
      appearance: "dark" as const,
      fontSizePx: 13,
      changeType: "edit" as const,
      diff: ["@@ -1,3 +1,3 @@", "-# Base document", "+# Draft document", " Ready to commit."].join("\n"),
      diffMessageId: "diff-markdown-sticky-mode",
      filePath: "/repo/README.md",
      gitSectionId: "unstaged" as const,
      language: "markdown",
      sessionId: "session-1",
      workspaceRoot: "/repo",
      onOpenPath: () => {},
      onSaveFile: async () => {},
      summary: "Updated README",
    };
    const { rerender } = render(
      <DiffPanel
        {...baseProps}
        documentContent={{
          before: {
            content: "# Base document\n\nReady to commit.\n",
            source: "index",
          },
          after: {
            content: "# Draft document\n\nReady to commit.\n",
            source: "worktree",
          },
          canEdit: true,
          isCompleteDocument: true,
        }}
      />,
    );

    await clickAndSettle(screen.getByRole("button", { name: "All lines" }));
    await clickAndSettle(screen.getByRole("button", { name: "Rendered Markdown" }));
    expect(screen.getByLabelText("Markdown diff status")).toBeInTheDocument();

    rerender(
      <DiffPanel
        {...baseProps}
        documentContent={null}
      />,
    );

    expect(screen.getByLabelText("Markdown diff status")).toBeInTheDocument();
    expect(screen.getAllByText("Patch preview").length).toBeGreaterThan(0);
  });

  it("treats Markdown patch fallback previews as incomplete and read-only", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Draft document\n\nReady to commit.\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1,3 +1,3 @@", "-# Base document", "+# Draft document", " Ready to commit."].join("\n")}
          diffMessageId="diff-markdown-patch-fallback"
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

    await clickAndSettle(screen.getByRole("button", { name: "Rendered Markdown" }));

    expect(screen.getAllByText("Patch preview").length).toBeGreaterThan(0);
    // The patch fallback note comes from `diff-preview.ts` and is plumbed into
    // `markdownPreview.after.note` before the DiffPanel fallback string ever
    // applies. Assert the actual rendered note text so a regression in either
    // layer fails this test.
    expect(
      screen.getByText("Preview reconstructed from the patch. Unchanged regions outside shown hunks are omitted."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Save Markdown" })).not.toBeInTheDocument();
    expect(document.querySelector("[data-markdown-editable='true']")).toBeNull();
  });

  it("shows Markdown enrichment notes and suppresses false line numbers for omitted patch context", async () => {
    fetchFileMock.mockResolvedValue({
      content: [
        "# Draft document",
        "Ready to commit.",
        "Middle context.",
        "Second draft section.",
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
            "@@ -10,2 +10,2 @@",
            "-# Base document",
            "+# Draft document",
            " Ready to commit.",
            "@@ -40,2 +40,2 @@",
            "-Second base section.",
            "+Second draft section.",
            " Tail context.",
          ].join("\n")}
          documentEnrichmentNote="Rendered Markdown is unavailable because the document exceeds the 10 MB read limit."
          diffMessageId="diff-markdown-patch-omitted-lines"
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

    await clickAndSettle(screen.getByRole("button", { name: "Rendered Markdown" }));

    expect(
      screen.getByText("Rendered Markdown is unavailable because the document exceeds the 10 MB read limit."),
    ).toBeInTheDocument();
    expect(screen.getByText("...")).toBeInTheDocument();
    await waitFor(() => {
      expect(document.querySelector(".markdown-line-gutter [data-markdown-gutter-line='10']")).not.toBeNull();
    });
    expect(document.querySelector(".markdown-line-gutter [data-markdown-gutter-line='1']")).toBeNull();
  });

  it("shows Markdown enrichment notes when the diff falls back to raw patch mode", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# README\n",
      language: "markdown",
      path: "/repo/README.md",
    });

    render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff="not a structured unified diff"
        documentEnrichmentNote="Rendered Markdown is unavailable due to a read error."
        diffMessageId="diff-markdown-raw-note"
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

    expect(await screen.findByTestId("monaco-code-editor")).toBeInTheDocument();
    await clickAndSettle(screen.getByRole("button", { name: "Raw patch" }));

    expect(await screen.findByRole("table", { name: "Raw patch preview" })).toBeInTheDocument();
    expect(
      screen.getByText("Rendered Markdown is unavailable due to a read error."),
    ).toBeInTheDocument();
  });

  it("passes rendered Markdown diff link metadata to the open-path callback", async () => {
    fetchFileMock.mockResolvedValue({
      content: "See [target](src/app.ts#L20C4).\n",
      language: "markdown",
      path: "/repo/README.md",
    });
    const onOpenPath = vi.fn();

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="create"
          diff={["@@ -0,0 +1 @@", "+See [target](src/app.ts#L20C4)."].join("\n")}
          documentContent={{
            before: {
              content: "",
              source: "empty",
            },
            after: {
              content: "See [target](src/app.ts#L20C4).\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-link-metadata"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={onOpenPath}
          onSaveFile={async () => {}}
          summary="Created README"
        />,
      );
    });

    fireEvent.click(await screen.findByRole("link", { name: "target" }));

    expect(onOpenPath).toHaveBeenCalledWith("/repo/src/app.ts", {
      line: 20,
      column: 4,
      openInNewTab: false,
    });
  });

  it("recomputes rendered Markdown diff sections after editing unchanged content", async () => {
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
          diffMessageId="diff-markdown-live-rediff"
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

    const normalSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-normal-section[data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThan(0);
      return sections;
    });
    expect(normalSections[1]).toHaveTextContent("Shared middle.");
    editRenderedMarkdownSection(normalSections[1], "<p>Shared center.</p>");
    fireEvent.blur(normalSections[1]);

    const removedSections = document.querySelectorAll(".markdown-diff-rendered-section-removed");
    const addedSections = document.querySelectorAll(".markdown-diff-rendered-section-added");
    expect(removedSections.length).toBeGreaterThan(0);
    expect(addedSections.length).toBeGreaterThan(0);
    expect(Array.from(removedSections).some((section) => section.textContent?.includes("Shared middle."))).toBe(true);
    expect(Array.from(addedSections).some((section) => section.textContent?.includes("Shared center."))).toBe(true);
  });

  it("uses the same editable buffer when switching between code and rendered Markdown modes", async () => {
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
          diffMessageId="diff-markdown-shared-buffer"
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

    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));
    await changeAndSettle(await screen.findByTestId("monaco-code-editor"), {
      target: {
        value: "Shared intro.\n# Draft document\nShared center.\nReady to ship.\nShared outro.\n",
      },
    });
    await clickAndSettle(screen.getByRole("button", { name: "Rendered Markdown" }));

    // Adjacent diff lines without a blank line between them render into a
    // single <p> element (e.g. "Shared center.\nReady to ship."), so we assert
    // the added section contains both the edited lines rather than looking
    // them up as isolated text nodes.
    const addedSection = document.querySelector<HTMLElement>(".markdown-diff-rendered-section-added");
    expect(addedSection).not.toBeNull();
    expect(addedSection?.textContent).toContain("Shared center.");
    expect(addedSection?.textContent).toContain("Ready to ship.");
    const removedSection = document.querySelector<HTMLElement>(".markdown-diff-rendered-section-removed");
    expect(removedSection).not.toBeNull();
    expect(removedSection?.textContent).toContain("Shared middle.");
    expect(removedSection?.textContent).toContain("Committed text.");
    expect(document.body.textContent).not.toContain("Ready to commit.");
  });

  it("does not turn newline-only Markdown differences into whole-document sections", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Task Management\n\nShared intro.\nInserted detail.\nShared outro.\n",
      language: "markdown",
      path: "/repo/docs/features/TASKS.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,4 +1,5 @@",
            " # Task Management",
            " ",
            " Shared intro.",
            "+Inserted detail.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Task Management\r\n\r\nShared intro.\r\nShared outro.\r\n",
              source: "index",
            },
            after: {
              content: "# Task Management\n\nShared intro.\nInserted detail.\nShared outro.\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-newline-normalized"
          filePath="/repo/docs/features/TASKS.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated tasks doc"
        />,
      );
    });

    await waitFor(() => {
      expect(document.querySelector(".markdown-diff-rendered-section-added")).not.toBeNull();
    });

    const removedSections = document.querySelectorAll(".markdown-diff-rendered-section-removed");
    const addedSections = document.querySelectorAll(".markdown-diff-rendered-section-added");
    expect(removedSections.length).toBe(0);
    expect(addedSections.length).toBe(1);
    expect(addedSections[0]).toHaveTextContent("Inserted detail.");
    expect(addedSections[0]).not.toHaveTextContent("Task Management");
  });

  it("renders Markdown link-only changes instead of hiding normalized matches", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Task Management2\n\n- [`lib/models/task_definition.dart`](lib/models/task_definition.dart) - TaskDefinition model\n\n## Overview\n",
      language: "markdown",
      path: "/repo/docs/features/TASKS.md",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            "-# Task Management",
            "+# Task Management2",
            " ",
            "- `lib/models/task_definition.dart` - TaskDefinition model",
            "+ [`lib/models/task_definition.dart`](lib/models/task_definition.dart) - TaskDefinition model",
            " ",
            " ## Overview",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Task Management\n\n- `lib/models/task_definition.dart` - TaskDefinition model\n\n## Overview\n",
              source: "index",
            },
            after: {
              content: "# Task Management2\n\n- [`lib/models/task_definition.dart`](lib/models/task_definition.dart) - TaskDefinition model\n\n## Overview\n",
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-render-equivalent-links"
          filePath="/repo/docs/features/TASKS.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated tasks doc"
        />,
      );
    });

    await waitFor(() => {
      expect(document.querySelector(".markdown-diff-rendered-section-added")).not.toBeNull();
    });

    const removedSections = document.querySelectorAll(".markdown-diff-rendered-section-removed");
    const addedSections = document.querySelectorAll(".markdown-diff-rendered-section-added");
    expect(removedSections.length).toBe(2);
    expect(addedSections.length).toBe(2);
    expect(removedSections[0]).toHaveTextContent("Task Management");
    expect(addedSections[0]).toHaveTextContent("Task Management2");
    expect(Array.from(removedSections).some((section) => section.textContent?.includes("task_definition.dart"))).toBe(true);
    expect(Array.from(addedSections).some((section) => section.textContent?.includes("task_definition.dart"))).toBe(true);
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

  // Regression: editable sections set `role="button"` on a container with
  // rich interactive descendants, which violates ARIA and collapses the
  // whole subtree into a single screen-reader label.
  it("does not mark editable Markdown sections with role=button", async () => {
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
      expect(section.getAttribute("role")).toBeNull();
    }
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
  it("preserves watcher-driven disk refreshes that arrive while a rendered Markdown section is editing", async () => {
    fetchFileMock
      .mockResolvedValueOnce({
        content: "# Title\n\nSection one original.\n\nSection two original.\n",
        language: "markdown",
        path: "/repo/notes.md",
      })
      .mockResolvedValueOnce({
        content: "# Title\n\nSection one original.\n\nSection two refined externally.\n",
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

    const baseProps = {
      appearance: "dark" as const,
      fontSizePx: 13,
      changeType: "edit" as const,
      diff: [
        "@@ -1,5 +1,5 @@",
        " # Title",
        " ",
        "-Section one base.",
        "+Section one original.",
        " ",
        "-Section two base.",
        "+Section two original.",
      ].join("\n"),
      documentContent: {
        before: {
          content: "# Title\n\nSection one base.\n\nSection two base.\n",
          source: "index" as const,
        },
        after: {
          content: "# Title\n\nSection one original.\n\nSection two original.\n",
          source: "worktree" as const,
        },
        canEdit: true,
        isCompleteDocument: true,
      },
      diffMessageId: "diff-markdown-watcher-rebase",
      filePath: "/repo/notes.md",
      gitSectionId: "unstaged" as const,
      language: "markdown",
      sessionId: "session-1",
      workspaceRoot: "/repo",
      onOpenPath: () => {},
      onSaveFile,
      summary: "Updated notes",
    };

    const { rerender } = render(
      <DiffPanel {...baseProps} workspaceFilesChangedEvent={null} />,
    );

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledTimes(1);
    });

    // Start editing section one in rendered mode (draft stays local).
    const addedSections = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThanOrEqual(2);
      return sections;
    });
    const sectionOne = Array.from(addedSections).find((section) =>
      section.textContent?.includes("Section one original."),
    );
    expect(sectionOne).toBeTruthy();
    editRenderedMarkdownSection(sectionOne!, "<p>Section one revised.</p>");

    // A watcher event arrives while the section is still being edited.
    // The on-disk content has refined Section two. The refresh effect runs
    // with `editValue === latestFile.content` (no propagation), so it takes
    // the "not dirty" branch, re-fetches the file, and updates the preview
    // while the user's in-progress local draft remains in the contentEditable
    // DOM until commit.
    rerender(
      <DiffPanel
        {...baseProps}
        documentContent={{
          ...baseProps.documentContent,
          after: {
            content: "# Title\n\nSection one original.\n\nSection two refined externally.\n",
            source: "worktree" as const,
          },
        }}
        workspaceFilesChangedEvent={{
          revision: 2,
          changes: [{ path: "/repo/notes.md", kind: "modified" }],
        }}
      />,
    );

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledTimes(2);
    });

    // Commit the rendered-mode draft. The commit must apply to the post-
    // rebase live content (Section two refined externally.), not to the
    // pre-rebase baseline that the section was first mounted against.
    const refreshedSection = await waitFor(() => {
      const section = Array.from(
        document.querySelectorAll<HTMLElement>(
          ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
        ),
      ).find((candidate) => candidate.textContent?.includes("Section one revised."));
      expect(section).toBeTruthy();
      return section!;
    });
    fireEvent.blur(refreshedSection);

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    // Saved content must contain BOTH the external watcher update AND the
    // rendered-mode draft. Before the fix, the stale frozen baseline
    // produced "# Title\n\nSection one revised.\n\nSection two original.\n"
    // (dropping the external refinement).
    expect(savedCapture.length).toBeGreaterThan(0);
    const latestSave = savedCapture[savedCapture.length - 1];
    expect(latestSave.path).toBe("/repo/notes.md");
    expect(latestSave.content).toBe(
      "# Title\n\nSection one revised.\n\nSection two refined externally.\n",
    );
  });

  it("rebases active rendered Markdown drafts when documentContent changes before the edited section", async () => {
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

    const baseProps = {
      appearance: "dark" as const,
      fontSizePx: 13,
      changeType: "edit" as const,
      diff: [
        "@@ -1,5 +1,5 @@",
        " # Title",
        " ",
        "-Section one base.",
        "+Section one original.",
        " ",
        "-Section two base.",
        "+Section two original.",
      ].join("\n"),
      documentContent: {
        before: {
          content: "# Title\n\nSection one base.\n\nSection two base.\n",
          source: "index" as const,
        },
        after: {
          content: "# Title\n\nSection one original.\n\nSection two original.\n",
          source: "worktree" as const,
        },
        canEdit: true,
        isCompleteDocument: true,
      },
      diffMessageId: "diff-markdown-document-content-rebase",
      filePath: "/repo/notes.md",
      gitSectionId: "unstaged" as const,
      language: "markdown",
      sessionId: "session-1",
      workspaceRoot: "/repo",
      onOpenPath: () => {},
      onSaveFile,
      summary: "Updated notes",
    };

    const { rerender } = render(
      <DiffPanel {...baseProps} workspaceFilesChangedEvent={null} />,
    );

    const sectionTwo = await waitFor(() => {
      const section = Array.from(
        document.querySelectorAll<HTMLElement>(
          ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
        ),
      ).find((candidate) => candidate.textContent?.includes("Section two original."));
      expect(section).toBeTruthy();
      return section!;
    });
    editRenderedMarkdownSection(sectionTwo, "<p>Section two local draft.</p>");

    rerender(
      <DiffPanel
        {...baseProps}
        documentContent={{
          ...baseProps.documentContent,
          before: {
            content: "# Title\n\nExternal intro.\n\nSection one base.\n\nSection two base.\n",
            source: "index" as const,
          },
          after: {
            content: "# Title\n\nExternal intro.\n\nSection one original.\n\nSection two original.\n",
            source: "worktree" as const,
          },
        }}
        workspaceFilesChangedEvent={null}
      />,
    );

    await waitFor(() => {
      expect(document.body).toHaveTextContent("External intro.");
    });

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(savedCapture.length).toBeGreaterThan(0);
    const latestSave = savedCapture[savedCapture.length - 1];
    expect(latestSave.path).toBe("/repo/notes.md");
    expect(latestSave.content).toBe(
      "# Title\n\nExternal intro.\n\nSection one original.\n\nSection two local draft.\n",
    );
  });

  it("preserves downstream repeated rendered Markdown drafts when an upstream duplicate is inserted", async () => {
    const initialBefore = [
      "# Title",
      "",
      "Intro context.",
      "",
      "Bridge context.",
      "",
      "Repeated base.",
      "",
      "Middle context.",
      "",
      "Repeated base.",
      "",
    ].join("\n");
    const initialAfter = [
      "# Title",
      "",
      "Intro context.",
      "",
      "Bridge context.",
      "",
      "Repeated original.",
      "",
      "Middle context.",
      "",
      "Repeated original.",
      "",
    ].join("\n");
    const refreshedAfter = [
      "# Title",
      "",
      "Intro context.",
      "",
      "Repeated original.",
      "",
      "Bridge context.",
      "",
      "Repeated original.",
      "",
      "Middle context.",
      "",
      "Repeated original.",
      "",
    ].join("\n");
    fetchFileMock.mockResolvedValue({
      content: initialAfter,
      language: "markdown",
      path: "/repo/notes.md",
    });
    const savedCapture: Array<{ path: string; content: string }> = [];
    const onSaveFile = vi.fn().mockImplementation(async (path, content) => {
      savedCapture.push({ path, content });
      return { content, language: "markdown", path };
    });

    const baseProps = {
      appearance: "dark" as const,
      fontSizePx: 13,
      changeType: "edit" as const,
      diff: [
        "@@ -1,12 +1,12 @@",
        " # Title",
        " ",
        " Intro context.",
        " ",
        " Bridge context.",
        " ",
        "-Repeated base.",
        "+Repeated original.",
        " ",
        " Middle context.",
        " ",
        "-Repeated base.",
        "+Repeated original.",
      ].join("\n"),
      documentContent: {
        before: { content: initialBefore, source: "index" as const },
        after: { content: initialAfter, source: "worktree" as const },
        canEdit: true,
        isCompleteDocument: true,
      },
      diffMessageId: "diff-markdown-repeated-stable-id",
      filePath: "/repo/notes.md",
      gitSectionId: "unstaged" as const,
      language: "markdown",
      sessionId: "session-1",
      workspaceRoot: "/repo",
      onOpenPath: () => {},
      onSaveFile,
      summary: "Updated notes",
    };

    const { rerender } = render(
      <DiffPanel {...baseProps} workspaceFilesChangedEvent={null} />,
    );

    const repeatedSections = await waitFor(() => {
      const candidates = Array.from(
        document.querySelectorAll<HTMLElement>(
          ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
        ),
      ).filter((section) => section.textContent?.includes("Repeated original."));
      expect(candidates).toHaveLength(2);
      return candidates;
    });
    const downstreamRepeatedSection = repeatedSections[1];
    expect(downstreamRepeatedSection).toBeTruthy();

    editRenderedMarkdownSection(
      downstreamRepeatedSection!,
      "<p>Repeated local draft.</p>",
    );

    rerender(
      <DiffPanel
        {...baseProps}
        documentContent={{
          ...baseProps.documentContent,
          after: { content: refreshedAfter, source: "worktree" as const },
        }}
        workspaceFilesChangedEvent={null}
      />,
    );

    await waitFor(() => {
      expect(document.body).toHaveTextContent("Repeated local draft.");
    });

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    expect(savedCapture.length).toBeGreaterThan(0);
    expect(savedCapture[savedCapture.length - 1]).toEqual({
      path: "/repo/notes.md",
      content: [
        "# Title",
        "",
        "Intro context.",
        "",
        "Repeated original.",
        "",
        "Bridge context.",
        "",
        "Repeated original.",
        "",
        "Middle context.",
        "",
        "Repeated local draft.",
        "",
      ].join("\n"),
    });
  });

  it("preserves rendered Markdown drafts when a watcher reports file deletion", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Title\n\nSection one original.\n",
      language: "markdown",
      path: "/repo/notes.md",
    });

    const baseProps = {
      appearance: "dark" as const,
      fontSizePx: 13,
      changeType: "edit" as const,
      diff: [
        "@@ -1,3 +1,3 @@",
        " # Title",
        " ",
        "-Section one base.",
        "+Section one original.",
      ].join("\n"),
      documentContent: {
        before: {
          content: "# Title\n\nSection one base.\n",
          source: "index" as const,
        },
        after: {
          content: "# Title\n\nSection one original.\n",
          source: "worktree" as const,
        },
        canEdit: true,
        isCompleteDocument: true,
      },
      diffMessageId: "diff-markdown-delete-draft",
      filePath: "/repo/notes.md",
      gitSectionId: "unstaged" as const,
      language: "markdown",
      sessionId: "session-1",
      workspaceRoot: "/repo",
      onOpenPath: () => {},
      onSaveFile: async () => {},
      summary: "Updated notes",
    };

    const { rerender } = render(
      <DiffPanel {...baseProps} workspaceFilesChangedEvent={null} />,
    );

    const section = await waitFor(() => {
      const candidate = document.querySelector<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(candidate).toBeTruthy();
      return candidate!;
    });
    editRenderedMarkdownSection(section, "<p>Section one local draft.</p>");

    rerender(
      <DiffPanel
        {...baseProps}
        workspaceFilesChangedEvent={{
          revision: 7,
          changes: [{ path: "/repo/notes.md", kind: "deleted" }],
        }}
      />,
    );

    expect(
      await screen.findByText("The file was deleted on disk. Your diff edit buffer is preserved."),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save anyway" })).toBeEnabled();
  });

  // Regression: rendered-mode edit handlers used `markdownPreview.after.content`
  // for their baseline, but the displayed segments were computed from the
  // dirty `editValue` buffer when the user had pending code-mode edits. The
  // offsets therefore pointed into a stale content string and silently
  // discarded the code-mode changes. The fix snapshots the segment source
  // content alongside the segments and passes it explicitly to the handlers.
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

  it("loads the latest file with a project scope when no session is present", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const latest = true;\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1 +1 @@", "-old line", "+new line"].join("\n")}
          diffMessageId="diff-project"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId={null}
          projectId="project-1"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      );
    });

    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledWith("/repo/src/example.ts", {
        sessionId: null,
        projectId: "project-1",
      });
    });

    expect(await screen.findByTestId("monaco-code-editor")).toHaveValue("const latest = true;\n");
  });

  it("refreshes the open diff file when a watcher event touches it", async () => {
    fetchFileMock
      .mockResolvedValueOnce({
        content: "const value = 'initial';\n",
        language: "typescript",
        path: "/repo/src/example.ts",
      })
      .mockResolvedValueOnce({
        content: "const value = 'external';\n",
        language: "typescript",
        path: "/repo/src/example.ts",
      });

    const { rerender } = render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1 +1 @@", "-const value = 'base';", "+const value = 'initial';"].join("\n")}
        diffMessageId="diff-watch"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        workspaceRoot="/repo"
        workspaceFilesChangedEvent={null}
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated example file"
      />,
    );

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledTimes(1);
    });
    expect(await screen.findByTestId("monaco-diff-editor-modified")).toHaveValue(
      "const value = 'initial';\n",
    );

    rerender(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1 +1 @@", "-const value = 'base';", "+const value = 'external';"].join("\n")}
        diffMessageId="diff-watch"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        workspaceRoot="/repo"
        workspaceFilesChangedEvent={{
          revision: 2,
          changes: [{ path: "/repo/src/example.ts", kind: "modified" }],
        }}
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated example file"
      />,
    );

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledTimes(2);
    });
    expect(await screen.findByTestId("monaco-diff-editor-modified")).toHaveValue(
      "const value = 'external';\n",
    );
    expect(screen.getByText("File refreshed from disk.")).toBeInTheDocument();
  });

  it("rebases dirty diff edits onto non-overlapping disk changes", async () => {
    fetchFileMock
      .mockResolvedValueOnce({
        content: "alpha\nbeta\n",
        language: "typescript",
        path: "/repo/src/example.ts",
      })
      .mockResolvedValueOnce({
        content: "alpha\nbeta disk\n",
        language: "typescript",
        path: "/repo/src/example.ts",
      });

    const { rerender } = render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,2 +1,2 @@", " alpha", "-beta base", "+beta"].join("\n")}
        diffMessageId="diff-rebase"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        workspaceRoot="/repo"
        workspaceFilesChangedEvent={null}
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated example file"
      />,
    );

    const modifiedEditor = await screen.findByTestId("monaco-diff-editor-modified");
    await changeAndSettle(modifiedEditor, {
      target: { value: "alpha local\nbeta\n" },
    });

    rerender(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,2 +1,2 @@", " alpha", "-beta base", "+beta disk"].join("\n")}
        diffMessageId="diff-rebase"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        workspaceRoot="/repo"
        workspaceFilesChangedEvent={{
          revision: 3,
          changes: [{ path: "/repo/src/example.ts", kind: "modified" }],
        }}
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated example file"
      />,
    );

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledTimes(2);
    });
    expect(await screen.findByTestId("monaco-diff-editor-modified")).toHaveValue(
      "alpha local\nbeta disk\n",
    );
    expect(
      screen.getByText("File changed on disk; your diff edits were applied on top."),
    ).toBeInTheDocument();
  });

  it("rebases dirty diff edits typed while the watcher refresh is in flight", async () => {
    const diskRefresh = createDeferred<{
      content: string;
      language: string;
      path: string;
    }>();
    fetchFileMock
      .mockResolvedValueOnce({
        content: "alpha\nbeta\n",
        language: "typescript",
        path: "/repo/src/example.ts",
      })
      .mockImplementationOnce(() => diskRefresh.promise);

    const { rerender } = render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,2 +1,2 @@", " alpha", "-beta base", "+beta"].join("\n")}
        diffMessageId="diff-rebase-late"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        workspaceRoot="/repo"
        workspaceFilesChangedEvent={null}
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated example file"
      />,
    );

    const modifiedEditor = await screen.findByTestId("monaco-diff-editor-modified");
    await changeAndSettle(modifiedEditor, {
      target: { value: "alpha local\nbeta\n" },
    });

    rerender(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,2 +1,2 @@", " alpha", "-beta base", "+beta disk"].join("\n")}
        diffMessageId="diff-rebase-late"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        workspaceRoot="/repo"
        workspaceFilesChangedEvent={{
          revision: 30,
          changes: [{ path: "/repo/src/example.ts", kind: "modified" }],
        }}
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated example file"
      />,
    );

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledTimes(2);
    });
    await changeAndSettle(modifiedEditor, {
      target: { value: "alpha local late\nbeta\n" },
    });

    await act(async () => {
      diskRefresh.resolve({
        content: "alpha\nbeta disk\n",
        language: "typescript",
        path: "/repo/src/example.ts",
      });
      await Promise.resolve();
    });

    expect(await screen.findByTestId("monaco-diff-editor-modified")).toHaveValue(
      "alpha local late\nbeta disk\n",
    );
  });

  it("preserves dirty diff edits when a watcher event reports deletion", async () => {
    fetchFileMock.mockResolvedValueOnce({
      content: "alpha\nbeta\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });

    const { rerender } = render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,2 +1,2 @@", " alpha", "-beta base", "+beta"].join("\n")}
        diffMessageId="diff-delete-watch"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        workspaceRoot="/repo"
        workspaceFilesChangedEvent={null}
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated example file"
      />,
    );

    const modifiedEditor = await screen.findByTestId("monaco-diff-editor-modified");
    await changeAndSettle(modifiedEditor, {
      target: { value: "alpha local\nbeta\n" },
    });

    rerender(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,2 +1,2 @@", " alpha", "-beta base", "+beta"].join("\n")}
        diffMessageId="diff-delete-watch"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        workspaceRoot="/repo"
        workspaceFilesChangedEvent={{
          revision: 4,
          changes: [{ path: "/repo/src/example.ts", kind: "deleted" }],
        }}
        onOpenPath={() => {}}
        onSaveFile={async () => {}}
        summary="Updated example file"
      />,
    );

    expect(await screen.findByTestId("monaco-diff-editor-modified")).toHaveValue(
      "alpha local\nbeta\n",
    );
    expect(
      screen.getByText("The file was deleted on disk. Your diff edit buffer is preserved."),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Apply my edits to disk version" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save anyway" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reload from disk" })).toBeInTheDocument();
  });

  it("renders plain added and removed stats with +/- markers", async () => {
    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "@@ -1,5 +1,5 @@",
            "-const before = false;",
            "+const after = true;",
            " unchanged",
            "+const added = true;",
            " stable",
            "-const removed = true;",
          ].join("\n")}
          diffMessageId="diff-stats"
          filePath={null}
          language="typescript"
          sessionId={null}
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      );
    });

    const addedStat = screen.getByLabelText("Added lines: 1");
    const removedStat = screen.getByLabelText("Removed lines: 1");

    expect(addedStat).toHaveTextContent("+1");
    expect(removedStat).toHaveTextContent("-1");
    expect(addedStat).not.toHaveClass("chip");
    expect(removedStat).not.toHaveClass("chip");
  });

  it("shows unsaved changes in edit mode", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const latest = true;\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1 +1 @@", "-old line", "+new line"].join("\n")}
          diffMessageId="diff-edit"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId="session-1"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      );
    });

    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));
    const editor = await screen.findByTestId("monaco-code-editor");
    await changeAndSettle(editor, { target: { value: "changed\n" } });

    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
  });

  it("supports editing and saving from the full diff view", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const latest = true;\n",
      contentHash: "sha256:base",
      language: "typescript",
      path: "/repo/src/example.ts",
    });
    const onSaveFile = vi.fn(async () => ({
      content: "const latest = false;\n",
      contentHash: "sha256:saved",
      language: "typescript",
      path: "/repo/src/example.ts",
    }));

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1 +1 @@", "-old line", "+new line"].join("\n")}
          diffMessageId="diff-visual-edit"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId="session-1"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated example file"
        />,
      );
    });

    const editor = await screen.findByTestId("monaco-diff-editor-modified");
    expect(editor).toHaveValue("const latest = true;\n");
    expect(editor).not.toHaveAttribute("readonly");

    await changeAndSettle(editor, { target: { value: "const latest = false;\n" } });
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    await clickAndSettle(screen.getByRole("button", { name: "Mock diff save" }));

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith("/repo/src/example.ts", "const latest = false;\n", {
        baseHash: "sha256:base",
      });
    });
  });

  it("saves the latest diff edit value before React effects flush", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const latest = true;\n",
      contentHash: "sha256:base",
      language: "typescript",
      path: "/repo/src/example.ts",
    });
    const onSaveFile = vi.fn(async () => ({
      content: "const latest = false;\n",
      contentHash: "sha256:saved",
      language: "typescript",
      path: "/repo/src/example.ts",
    }));

    render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1 +1 @@", "-old line", "+new line"].join("\n")}
        diffMessageId="diff-save-latest-ref"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        onOpenPath={() => {}}
        onSaveFile={onSaveFile}
        summary="Updated example file"
      />,
    );

    const editor = await screen.findByTestId("monaco-diff-editor-modified");
    await act(async () => {
      fireEvent.change(editor, { target: { value: "const latest = false;\n" } });
      fireEvent.click(screen.getByRole("button", { name: "Mock diff save" }));
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "/repo/src/example.ts",
        "const latest = false;\n",
        {
          baseHash: "sha256:base",
        },
      );
    });
  });

  it("offers recovery actions after stale diff edit saves", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const latest = true;\n",
      contentHash: "sha256:base",
      language: "typescript",
      path: "/repo/src/example.ts",
    });
    const onSaveFile = vi
      .fn()
      .mockRejectedValueOnce(new Error("file changed on disk before save"))
      .mockResolvedValueOnce({
        content: "const latest = false;\n",
        contentHash: "sha256:mine",
        language: "typescript",
        path: "/repo/src/example.ts",
      });

    render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1 +1 @@", "-old line", "+new line"].join("\n")}
        diffMessageId="diff-stale-save"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        onOpenPath={() => {}}
        onSaveFile={onSaveFile}
        summary="Updated example file"
      />,
    );

    const editor = await screen.findByTestId("monaco-diff-editor-modified");
    await changeAndSettle(editor, { target: { value: "const latest = false;\n" } });
    await clickAndSettle(screen.getByRole("button", { name: "Mock diff save" }));

    expect(await screen.findByText("Save failed")).toBeInTheDocument();
    expect(screen.getByText(/file changed on disk before save/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Apply my edits to disk version" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reload from disk" })).toBeInTheDocument();

    await clickAndSettle(screen.getByRole("button", { name: "Save anyway" }));

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenLastCalledWith(
        "/repo/src/example.ts",
        "const latest = false;\n",
        {
          baseHash: "sha256:base",
          overwrite: true,
        },
      );
    });
  });

  it("applies stale diff edits to the latest disk version", async () => {
    fetchFileMock
      .mockResolvedValueOnce({
        content: "alpha\nbeta\n",
        contentHash: "sha256:base",
        language: "typescript",
        path: "/repo/src/example.ts",
      })
      .mockResolvedValueOnce({
        content: "alpha\nbeta disk\n",
        contentHash: "sha256:disk",
        language: "typescript",
        path: "/repo/src/example.ts",
      });
    const onSaveFile = vi
      .fn()
      .mockRejectedValueOnce(new Error("file changed on disk before save"));

    render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,2 +1,2 @@", " alpha", "-beta base", "+beta"].join("\n")}
        diffMessageId="diff-stale-apply"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        onOpenPath={() => {}}
        onSaveFile={onSaveFile}
        summary="Updated example file"
      />,
    );

    const editor = await screen.findByTestId("monaco-diff-editor-modified");
    await changeAndSettle(editor, { target: { value: "alpha local\nbeta\n" } });
    await clickAndSettle(screen.getByRole("button", { name: "Mock diff save" }));
    await clickAndSettle(
      await screen.findByRole("button", { name: "Apply my edits to disk version" }),
    );

    expect(await screen.findByTestId("monaco-diff-editor-modified")).toHaveValue(
      "alpha local\nbeta disk\n",
    );
    expect(
      screen.getByText("Your diff edits were applied on top of the disk version."),
    ).toBeInTheDocument();
  });

  it("reloads stale diff edits from disk on request", async () => {
    fetchFileMock
      .mockResolvedValueOnce({
        content: "alpha\nbeta\n",
        contentHash: "sha256:base",
        language: "typescript",
        path: "/repo/src/example.ts",
      })
      .mockResolvedValueOnce({
        content: "alpha disk\nbeta disk\n",
        contentHash: "sha256:disk",
        language: "typescript",
        path: "/repo/src/example.ts",
      });
    const onSaveFile = vi
      .fn()
      .mockRejectedValueOnce(new Error("file changed on disk before save"));

    render(
      <DiffPanel
        appearance="dark"
        fontSizePx={13}
        changeType="edit"
        diff={["@@ -1,2 +1,2 @@", " alpha", "-beta base", "+beta"].join("\n")}
        diffMessageId="diff-stale-reload"
        filePath="/repo/src/example.ts"
        language="typescript"
        sessionId="session-1"
        onOpenPath={() => {}}
        onSaveFile={onSaveFile}
        summary="Updated example file"
      />,
    );

    const editor = await screen.findByTestId("monaco-diff-editor-modified");
    await changeAndSettle(editor, { target: { value: "alpha local\nbeta\n" } });
    await clickAndSettle(screen.getByRole("button", { name: "Mock diff save" }));
    await clickAndSettle(await screen.findByRole("button", { name: "Reload from disk" }));

    expect(await screen.findByTestId("monaco-diff-editor-modified")).toHaveValue(
      "alpha disk\nbeta disk\n",
    );
    expect(screen.getByText("File reloaded from disk.")).toBeInTheDocument();
  });

  it("renders a color-coded raw patch view", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const latest = true;\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });

    let container!: HTMLElement;

    await act(async () => {
      ({ container } = render(
        <DiffPanel
          appearance="light"
          fontSizePx={13}
          changeType="edit"
          diff={[
            "diff --git a/example.ts b/example.ts",
            "@@ -1 +1 @@",
            "-old line",
            "+new line",
          ].join("\n")}
          diffMessageId="diff-2"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId="session-1"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      ));
    });

    await clickAndSettle(screen.getByRole("button", { name: "Raw patch" }));

    expect(container.querySelector(".diff-preview-raw-line-added")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-removed")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-hunk")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-meta")).not.toBeNull();
  });

  it("renders inline change emphasis in changed-only mode", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const greeting = 'hi';\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1 +1 @@", "-const greeting = 'hello';", "+const greeting = 'hi';"].join("\n")}
          diffMessageId="diff-3"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId="session-1"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Refined greeting"
        />,
      );
    });

    await clickAndSettle(screen.getByRole("button", { name: "Changed only" }));

    expect(await screen.findByTestId("structured-diff-view")).toBeInTheDocument();
    expect(document.querySelectorAll(".structured-diff-inline-change").length).toBeGreaterThan(0);
  });

  it("renders saved review threads inline in changed-only mode", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const greeting = 'hi';\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });
    fetchReviewDocumentMock.mockResolvedValue({
      reviewFilePath: "/repo/.termal/reviews/change-diff-threads.json",
      review: {
        version: 1,
        revision: 2,
        changeSetId: "change-diff-threads",
        threads: [
          {
            id: "thread-1",
            anchor: {
              kind: "line",
              filePath: "/repo/src/example.ts",
              hunkHeader: "@@ -1 +1 @@",
              oldLine: null,
              newLine: 1,
            },
            status: "open",
            comments: [
              {
                id: "comment-1",
                author: "agent",
                body: "Please split this into a named helper.",
                createdAt: "2026-03-17T22:00:00Z",
                updatedAt: "2026-03-17T22:00:00Z",
              },
              {
                id: "comment-2",
                author: "agent",
                body: "Handled in a follow-up patch.",
                createdAt: "2026-03-17T22:05:00Z",
                updatedAt: "2026-03-17T22:05:00Z",
              },
            ],
          },
        ],
      },
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          changeSetId="change-diff-threads"
          diff={["@@ -1 +1 @@", "-const greeting = 'hello';", "+const greeting = 'hi';"].join("\n")}
          diffMessageId="diff-threaded"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId="session-1"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Refined greeting"
        />,
      );
    });

    await clickAndSettle(screen.getByRole("button", { name: "Changed only" }));

    expect(await screen.findByText("Please split this into a named helper.")).toBeInTheDocument();
    expect(screen.getByText("Handled in a follow-up patch.")).toBeInTheDocument();
    expect(fetchReviewDocumentMock).toHaveBeenCalledWith("change-diff-threads", {
      sessionId: "session-1",
      projectId: null,
    });
  });

  it("creates a line review thread and persists it", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const greeting = 'hi';\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });
    fetchReviewDocumentMock.mockResolvedValue({
      reviewFilePath: "/repo/.termal/reviews/change-create-thread.json",
      review: {
        version: 1,
        revision: 0,
        changeSetId: "change-create-thread",
        threads: [],
      },
    });
    saveReviewDocumentMock.mockImplementation(async (_changeSetId, review) => ({
      reviewFilePath: "/repo/.termal/reviews/change-create-thread.json",
      review,
    }));

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          changeSetId="change-create-thread"
          diff={["@@ -1 +1 @@", "-const greeting = 'hello';", "+const greeting = 'hi';"].join("\n")}
          diffMessageId="diff-create-thread"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId="session-1"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Refined greeting"
        />,
      );
    });

    await clickAndSettle(screen.getByRole("button", { name: "Changed only" }));
    await clickAndSettle(screen.getByRole("button", { name: "Comment on line 1" }));
    await changeAndSettle(screen.getByPlaceholderText("Write a review comment..."), {
      target: { value: "Please factor this into a helper." },
    });
    await clickAndSettle(screen.getByRole("button", { name: "Start thread" }));

    await waitFor(() => {
      expect(saveReviewDocumentMock).toHaveBeenCalledTimes(1);
    });

    expect(saveReviewDocumentMock).toHaveBeenCalledWith(
      "change-create-thread",
      expect.objectContaining({
        changeSetId: "change-create-thread",
        revision: 0,
        files: [{ filePath: "/repo/src/example.ts", changeType: "edit" }],
        threads: [
          expect.objectContaining({
            anchor: {
              kind: "line",
              filePath: "/repo/src/example.ts",
              hunkHeader: "@@ -1 +1 @@",
              oldLine: 1,
              newLine: 1,
            },
            status: "open",
            comments: [
              expect.objectContaining({
                author: "user",
                body: "Please factor this into a helper.",
              }),
            ],
          }),
        ],
      }),
      {
        sessionId: "session-1",
        projectId: null,
      },
    );
  });

  it("inserts the review handoff prompt for open threads", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const greeting = 'hi';\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });
    fetchReviewDocumentMock.mockResolvedValue({
      reviewFilePath: "/repo/.termal/reviews/change-insert-review.json",
      review: {
        version: 1,
        revision: 4,
        changeSetId: "change-insert-review",
        threads: [
          {
            id: "thread-1",
            anchor: {
              kind: "line",
              filePath: "/repo/src/example.ts",
              hunkHeader: "@@ -1 +1 @@",
              oldLine: 1,
              newLine: 1,
            },
            status: "open",
            comments: [
              {
                id: "comment-1",
                author: "agent",
                body: "Please tighten this up.",
                createdAt: "2026-03-17T22:00:00Z",
                updatedAt: "2026-03-17T22:00:00Z",
              },
            ],
          },
        ],
      },
    });
    const onInsertReviewIntoPrompt = vi.fn();

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          changeSetId="change-insert-review"
          diff={["@@ -1 +1 @@", "-const greeting = 'hello';", "+const greeting = 'hi';"].join("\n")}
          diffMessageId="diff-insert-review"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId="session-1"
          onInsertReviewIntoPrompt={onInsertReviewIntoPrompt}
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Refined greeting"
        />,
      );
    });

    await clickAndSettle(await screen.findByRole("button", { name: "Insert review into prompt" }));

    expect(onInsertReviewIntoPrompt).toHaveBeenCalledWith(
      "/repo/.termal/reviews/change-insert-review.json",
      "Please address the 1 open review thread in /repo/.termal/reviews/change-insert-review.json. Reply in each thread and resolve threads you have handled.",
    );
  });

  it("disables review actions when the review document fails to load", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const greeting = 'hi';\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });
    fetchReviewDocumentMock.mockRejectedValue(new Error("failed to parse review file"));

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          changeSetId="change-bad-review"
          diff={["@@ -1 +1 @@", "-const greeting = 'hello';", "+const greeting = 'hi';"].join("\n")}
          diffMessageId="diff-bad-review"
          filePath="/repo/src/example.ts"
          language="typescript"
          sessionId="session-1"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Refined greeting"
        />,
      );
    });

    await clickAndSettle(screen.getByRole("button", { name: "Changed only" }));

    expect(await screen.findByText("Review threads unavailable: failed to parse review file")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Comment on change set" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Comment on line 1" })).not.toBeInTheDocument();
  });
});
