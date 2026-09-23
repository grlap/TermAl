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

  it("renders submodule diffs as read-only nested raw patches", async () => {
    const nestedPatch = [
      "Submodule modules/demo contains modified content",
      "diff --git a/modules/demo/file.txt b/modules/demo/file.txt",
      "index df967b9..2ecd216 100644",
      "--- a/modules/demo/file.txt",
      "+++ b/modules/demo/file.txt",
      "@@ -1 +1,2 @@",
      " base",
      "+worktree",
      "diff --git a/modules/demo/other.txt b/modules/demo/other.txt",
      "index 7898192..422c2b7 100644",
      "--- a/modules/demo/other.txt",
      "+++ b/modules/demo/other.txt",
      "@@ -1 +1,2 @@",
      " other",
      "+second change",
    ].join("\n");

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={nestedPatch}
          diffMessageId="diff-submodule"
          displayPath="/repo/modules/demo"
          filePath={null}
          gitSectionId="unstaged"
          language="git-submodule"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Unstaged submodule changes in modules/demo"
        />,
      );
    });

    expect(screen.getByText("Submodule")).toHaveClass("chip");
    expect(screen.getByRole("table", { name: "Raw patch preview" })).toBeInTheDocument();
    expect(
      screen.getByText("diff --git a/modules/demo/file.txt b/modules/demo/file.txt"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit mode" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open file" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "All lines" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Changed only" })).not.toBeInTheDocument();
    expect(screen.queryByTestId("structured-diff-view")).not.toBeInTheDocument();
    expect(screen.getByText("modules/demo")).toBeInTheDocument();
    expect(fetchFileMock).not.toHaveBeenCalled();
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

  // Phase 4 of `docs/features/source-renderers.md`: non-Markdown
  // files with renderable regions (e.g., `.mmd` Mermaid files) get
  // a read-only "Rendered" diff view that composes the detected
  // regions via MarkdownContent's existing safe Mermaid/KaTeX
  // rendering path.
  it("exposes a Rendered mode for `.mmd` diffs with a complete-document after side", async () => {
    fetchFileMock.mockResolvedValue({
      content: "flowchart TD\n  A --> B\n",
      language: null,
      path: "/repo/diagrams/flow.mmd",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1 +1 @@", "-flowchart TD", "+flowchart TD", "  A --> B"].join("\n")}
          diffMessageId="diff-mmd"
          filePath="/repo/diagrams/flow.mmd"
          documentContent={{
            before: {
              content: "flowchart TD\n",
              source: "worktree",
            },
            after: {
              content: "flowchart TD\n  A --> B\n",
              source: "worktree",
            },
            isCompleteDocument: true,
            canEdit: true,
            editBlockedReason: null,
            note: null,
          }}
          gitSectionId="unstaged"
          language={null}
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated diagram"
        />,
      );
    });

    // The "Rendered" toggle is present because the registry detected
    // a whole-file Mermaid region on the after side.
    const renderedButton = screen.getByRole("button", { name: "Rendered" });
    expect(renderedButton).toBeInTheDocument();
    // "Rendered Markdown" is NOT shown because `.mmd` is not a
    // Markdown target — the two modes are mutually exclusive.
    expect(screen.queryByRole("button", { name: "Rendered Markdown" })).toBeNull();

    await clickAndSettle(renderedButton);

    // The complete-document path must NOT label the preview
    // "Patch-only rendering". That banner is reserved for the
    // fallback case at line ~431 where `documentContent` is
    // missing. A regression that flipped the gating logic (e.g.,
    // rendering the banner unconditionally) would pass the
    // positive assertion in the sibling test without this
    // negative assertion here.
    expect(
      screen.queryByText(/Patch-only rendering/i),
    ).not.toBeInTheDocument();
    // The view renders a synthetic Markdown fragment; the regions'
    // line-range header should appear.
    expect(screen.getByText(/Lines 1[–-]3/)).toBeInTheDocument();
    // The underlying Mermaid renderer was invoked for the fence.
    await waitFor(() => {
      expect(mermaidRenderMock).toHaveBeenCalled();
    });
  });

  it("labels the Rendered diff preview as Patch-only when documentContent is missing", async () => {
    fetchFileMock.mockResolvedValue({
      content: "flowchart TD\n  A --> B\n",
      language: null,
      path: "/repo/diagrams/flow.mmd",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1 +1 @@", "-flowchart TD", "+flowchart TD", "  A --> B"].join("\n")}
          diffMessageId="diff-mmd-patch-only"
          filePath="/repo/diagrams/flow.mmd"
          // No documentContent prop at all — the backend did not
          // enrich the diff with the full before/after sides.
          gitSectionId="unstaged"
          language={null}
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated diagram"
        />,
      );
    });

    // With `fetchFileMock` supplying the worktree content, the
    // registry still finds the Mermaid region, so the Rendered
    // button surfaces. But the preview labels itself Patch-only
    // because `documentContent.isCompleteDocument` was not set.
    const renderedButton = await screen.findByRole("button", { name: "Rendered" });
    await clickAndSettle(renderedButton);
    expect(screen.getByText(/Patch-only rendering/i)).toBeInTheDocument();
  });

  // Regression guard for the "Rendered-diff fallback uses worktree
  // content for staged diffs" bug in docs/bugs.md. Before the fix,
  // `renderedDiffAfterContent` fell back to `latestFile.content`
  // (the current worktree) when `documentContent` was missing,
  // regardless of whether `gitSectionId` was "staged" or "unstaged".
  // On a staged diff whose worktree had unrelated unstaged edits,
  // the Rendered view showed the WORKTREE — not the index — which
  // silently misrepresented the side under review. The fix derives
  // the fallback from a patch-only `buildDiffPreviewModel` call so
  // the Rendered preview always matches the hunk's after-side.
  it("renders the staged after-side from the patch when documentContent is missing", async () => {
    // Worktree carries an unrelated unstaged edit ("flowchart TD /
    // X --> Y") that the diff does NOT describe. The diff's
    // staged after-side is a single line "flowchart LR". Before
    // the fix the Rendered view would feed Mermaid the full
    // worktree; with the fix it feeds the patch's after-side.
    fetchFileMock.mockResolvedValue({
      content: "flowchart TD\n  X --> Y\n",
      language: null,
      path: "/repo/diagrams/flow.mmd",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
          fontSizePx={13}
          changeType="edit"
          diff={["@@ -1 +1 @@", "-flowchart OLD", "+flowchart LR"].join("\n")}
          diffMessageId="diff-mmd-staged-patch-fallback"
          filePath="/repo/diagrams/flow.mmd"
          // No documentContent — backend didn't enrich the diff.
          gitSectionId="staged"
          language={null}
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Staged Mermaid update"
        />,
      );
    });

    const renderedButton = await screen.findByRole("button", { name: "Rendered" });
    await clickAndSettle(renderedButton);
    // Patch-only banner still appears because documentContent was
    // missing — the fix preserves the "best-effort" framing, it
    // just makes the best-effort faithful to the patch instead of
    // leaking the worktree.
    expect(screen.getByText(/Patch-only rendering/i)).toBeInTheDocument();
    // The mermaid renderer was called with the patch-derived
    // after-side, not the worktree. `flattenPreviewText` joins
    // hunk-right lines; a single `+flowchart LR` hunk → exactly
    // `"flowchart LR"` (no trailing newline). The worktree's
    // `"X --> Y"` token must NOT appear in any render call.
    await waitFor(() => {
      expect(mermaidRenderMock).toHaveBeenCalled();
    });
    const renderedSources = mermaidRenderMock.mock.calls.map(([, source]) => source);
    expect(renderedSources.some((source) => source.includes("flowchart LR"))).toBe(
      true,
    );
    expect(renderedSources.every((source) => !source.includes("X --> Y"))).toBe(
      true,
    );
    expect(renderedSources.every((source) => !source.includes("flowchart TD"))).toBe(
      true,
    );
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
    expect(screen.queryByRole("button", { name: "After" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Before" })).not.toBeInTheDocument();
  });

  it("navigates between rendered Markdown change blocks via prev/next controls and a counter", async () => {
    // Bug ledger: "Rendered Markdown diff view cannot jump between
    // changes" — the regular Monaco file diff has prev/next change
    // navigation; the rendered Markdown view did not. The new prev/
    // next controls are wired through `computeMarkdownDiffChangeBlocks`
    // so navigation stops match the visible change blocks 1:1, and
    // each block carries `data-markdown-diff-change-index="N"` so the
    // scroll handler can find it. Two changed line replacements (line
    // 2: "Base document" → "Staged document"; line 4: "Committed
    // text." → "Ready to commit.") produce two change blocks.
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Staged document\nShared middle.\nReady to commit.\nShared outro.\n",
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
          diffMessageId="diff-markdown-change-nav"
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

    // Both change blocks rendered with the data-attribute so the
    // navigation scroll-into-view handler can target them.
    const blocks = await waitFor(() => {
      const matches = document.querySelectorAll<HTMLElement>(
        "[data-markdown-diff-change-index]",
      );
      expect(matches.length).toBe(2);
      return matches;
    });
    expect(blocks[0]?.getAttribute("data-markdown-diff-change-index")).toBe("0");
    expect(blocks[1]?.getAttribute("data-markdown-diff-change-index")).toBe("1");

    // Counter starts at "Change 1 of 2" and is exposed as a polite
    // live region so assistive tech announces navigation updates.
    const changeCounter = screen.getByText("Change 1 of 2");
    expect(changeCounter).toBeInTheDocument();
    expect(changeCounter).toHaveAttribute("aria-live", "polite");
    expect(changeCounter).toHaveAttribute("aria-atomic", "true");
    const originalScrollIntoViewDescriptor = Object.getOwnPropertyDescriptor(
      Element.prototype,
      "scrollIntoView",
    );
    const originalHtmlScrollIntoViewDescriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "scrollIntoView",
    );
    const scrollIntoViewCalls: Array<{
      element: Element;
      options?: boolean | ScrollIntoViewOptions;
    }> = [];
    const recordScrollIntoView = function recordScrollIntoView(
      this: Element,
      options?: boolean | ScrollIntoViewOptions,
    ) {
      scrollIntoViewCalls.push({ element: this, options });
    };
    const scrolledChangeIndexes = () =>
      scrollIntoViewCalls
        .filter(
          ({ options }) =>
            typeof options === "object" &&
            options !== null &&
            options.block === "center",
        )
        .map(({ element }) =>
          (element as HTMLElement).dataset.markdownDiffChangeIndex ?? null,
        );

    try {
      Object.defineProperty(Element.prototype, "scrollIntoView", {
        configurable: true,
        value: recordScrollIntoView,
      });
      Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
        configurable: true,
        value: recordScrollIntoView,
      });

      // Next: 1 -> 2, second block scrolled into view.
      await clickAndSettle(screen.getByRole("button", { name: "Next change" }));
      expect(screen.getByText("Change 2 of 2")).toBeInTheDocument();
      expect(scrolledChangeIndexes()).toEqual(["1"]);

      // Next at the end wraps to 1, first block scrolled into view.
      await clickAndSettle(screen.getByRole("button", { name: "Next change" }));
      expect(screen.getByText("Change 1 of 2")).toBeInTheDocument();
      expect(scrolledChangeIndexes()).toEqual(["1", "0"]);

      // Previous: 1 wraps to 2.
      await clickAndSettle(screen.getByRole("button", { name: "Previous change" }));
      expect(screen.getByText("Change 2 of 2")).toBeInTheDocument();
      expect(scrolledChangeIndexes()).toEqual(["1", "0", "1"]);

      // Previous: 2 -> 1.
      await clickAndSettle(screen.getByRole("button", { name: "Previous change" }));
      expect(screen.getByText("Change 1 of 2")).toBeInTheDocument();
      expect(scrolledChangeIndexes()).toEqual(["1", "0", "1", "0"]);
    } finally {
      if (originalScrollIntoViewDescriptor) {
        Object.defineProperty(
          Element.prototype,
          "scrollIntoView",
          originalScrollIntoViewDescriptor,
        );
      } else {
        delete (Element.prototype as Partial<Element>).scrollIntoView;
      }
      if (originalHtmlScrollIntoViewDescriptor) {
        Object.defineProperty(
          HTMLElement.prototype,
          "scrollIntoView",
          originalHtmlScrollIntoViewDescriptor,
        );
      } else {
        delete (HTMLElement.prototype as Partial<HTMLElement>).scrollIntoView;
      }
    }
  });

  it("scrolls the lone Markdown diff change into view when prev/next wraps to the same index", async () => {
    // Regression: with `changeCount === 1`,
    // prev/next compute the same index (0 -> 0); React bails on the
    // no-op state set, the scroll effect does not re-run, and the
    // controls appear dead. The fix advances a `navigationTick` on
    // every prev/next press so the scroll effect fires regardless of
    // whether `currentChangeIndex` changed.
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Staged document\nShared outro.\n",
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
            "@@ -1,3 +1,3 @@",
            " Shared intro.",
            "-# Base document",
            "+# Staged document",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content: "Shared intro.\n# Base document\nShared outro.\n",
              source: "head",
            },
            after: {
              content: "Shared intro.\n# Staged document\nShared outro.\n",
              source: "index",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-single-change-nav"
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

    // Exactly one change block, the counter says "Change 1 of 1".
    await waitFor(() => {
      const matches = document.querySelectorAll<HTMLElement>(
        "[data-markdown-diff-change-index]",
      );
      expect(matches.length).toBe(1);
    });
    expect(screen.getByText("Change 1 of 1")).toBeInTheDocument();

    const originalScrollIntoViewDescriptor = Object.getOwnPropertyDescriptor(
      Element.prototype,
      "scrollIntoView",
    );
    const originalHtmlScrollIntoViewDescriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "scrollIntoView",
    );
    const scrollIntoViewCalls: Array<{
      element: Element;
      options?: boolean | ScrollIntoViewOptions;
    }> = [];
    const recordScrollIntoView = function recordScrollIntoView(
      this: Element,
      options?: boolean | ScrollIntoViewOptions,
    ) {
      scrollIntoViewCalls.push({ element: this, options });
    };
    const scrolledChangeIndexes = () =>
      scrollIntoViewCalls
        .filter(
          ({ options }) =>
            typeof options === "object" &&
            options !== null &&
            options.block === "center",
        )
        .map(({ element }) =>
          (element as HTMLElement).dataset.markdownDiffChangeIndex ?? null,
        );

    try {
      Object.defineProperty(Element.prototype, "scrollIntoView", {
        configurable: true,
        value: recordScrollIntoView,
      });
      Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
        configurable: true,
        value: recordScrollIntoView,
      });

      // Next: the lone block scrolls into view even though the index
      // wraps from 0 to 0.
      await clickAndSettle(screen.getByRole("button", { name: "Next change" }));
      expect(screen.getByText("Change 1 of 1")).toBeInTheDocument();
      expect(scrolledChangeIndexes()).toEqual(["0"]);

      // Previous: same — scrolls again on the same lone block.
      await clickAndSettle(screen.getByRole("button", { name: "Previous change" }));
      expect(screen.getByText("Change 1 of 1")).toBeInTheDocument();
      expect(scrolledChangeIndexes()).toEqual(["0", "0"]);
    } finally {
      if (originalScrollIntoViewDescriptor) {
        Object.defineProperty(
          Element.prototype,
          "scrollIntoView",
          originalScrollIntoViewDescriptor,
        );
      } else {
        delete (Element.prototype as Partial<Element>).scrollIntoView;
      }
      if (originalHtmlScrollIntoViewDescriptor) {
        Object.defineProperty(
          HTMLElement.prototype,
          "scrollIntoView",
          originalHtmlScrollIntoViewDescriptor,
        );
      } else {
        delete (HTMLElement.prototype as Partial<HTMLElement>).scrollIntoView;
      }
    }
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
        document.querySelectorAll(
          ".markdown-diff-rendered-section-added .mermaid-diagram-frame",
        ).length,
      ).toBeGreaterThanOrEqual(1);
    });
  });

  it("keeps oversized Mermaid source visible in editable rendered Markdown diffs", async () => {
    const oversizedMermaidSource = `flowchart TD\n  ${"A".repeat(50_001)} --> B`;
    const afterContent = [
      "# Diagram",
      "",
      "```mermaid",
      oversizedMermaidSource,
      "```",
      "",
    ].join("\n");
    fetchFileMock.mockResolvedValue({
      content: afterContent,
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
            ...oversizedMermaidSource.split("\n").map((line) => `+${line}`),
            "+```",
          ].join("\n")}
          documentContent={{
            before: {
              content: "# Diagram\n\n",
              source: "index",
            },
            after: {
              content: afterContent,
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-mermaid-budget-source"
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

    expect(
      await screen.findByText(
        "Mermaid render skipped: diagram exceeds the 50,000 character render budget.",
      ),
    ).toBeInTheDocument();
    const editableSection = document.querySelector<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    );
    const sourceBlock = editableSection?.querySelector<HTMLElement>("code.language-mermaid");
    expect(sourceBlock?.textContent).toBe(oversizedMermaidSource);
    expect(screen.queryByTestId("mermaid-frame")).not.toBeInTheDocument();
    expect(mermaidRenderMock).not.toHaveBeenCalled();
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

    // IMMEDIATE-STATE assertion (load-bearing for the
    // raw-source-flash fix): the disallowed-input path must NOT
    // write the segment's raw markdown source into the
    // contentEditable subtree. The previous implementation ran
    // `event.currentTarget.textContent = segment.markdown` inline,
    // snapping the DOM from the user's mutation ("Ready to save.")
    // to the raw markdown source ("Ready to commit." — identical
    // shape here because the rendered Markdown source happens to
    // match the surface text, but structurally it was plain text
    // replacing the `<p>` wrapper). That write produced a visible
    // one-frame plain-source flash before the
    // `readOnlyResetVersion` remount repainted under React.
    //
    // The current fix leaves the user's mutation in place and
    // relies on the follow-up remount to restore the rendered
    // DOM. Assert that invariant here, BEFORE `waitFor` yields
    // to React's remount cycle:
    //   - The user-typed `<p>` wrapper is still present.
    //   - The section is NOT a plain text node whose textContent
    //     equals the raw source (the shape the reintroduced
    //     assignment would leave behind).
    //
    // Reverting the fix (restoring the `textContent = segment.
    // markdown` line in `markdown-diff-change-section.tsx`) makes
    // the first assertion fail — textContent collapses to the
    // raw source without the `<p>` wrapper — so this pair is
    // load-bearing for the exact regression the fix prevents.
    expect(addedSections[1].querySelector("p")).not.toBeNull();
    expect(addedSections[1].textContent).toContain("Ready to save.");

    // The read-only input handler bumps `readOnlyResetVersion` on
    // the parent, which remounts `MarkdownDiffDocument` via its
    // `key={readOnlyResetVersion}`. After remount, the OLD
    // `addedSections[1]` reference is detached from the DOM; we
    // must re-query to see the restored rendered Markdown. The
    // pre-refactor path assigned `event.currentTarget.textContent
    // = segment.markdown` inline BEFORE the remount, which made
    // this test pass against the stale reference — but also
    // produced a visible one-frame plain-source flash in
    // production. See docs/bugs.md preamble for the retirement.
    await waitFor(() => {
      const restoredAddedSections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-caret='true']",
      );
      // Pin the structural invariant alongside the content
      // assertion so a future test change that inserts an extra
      // added section before this assertion can't silently drift
      // the `[1]` index onto an unrelated element that happens to
      // contain matching text.
      expect(restoredAddedSections).toHaveLength(2);
      expect(restoredAddedSections[1]).toHaveTextContent("Ready to commit.");
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
    expect(screen.queryByText("Patch preview")).not.toBeInTheDocument();
  });

  it("renders complete large Markdown diffs while deferring editable full-document sections", async () => {
    const sharedLines = Array.from({ length: 1_205 }, (_, index) => `Shared line ${index + 1}`);
    const beforeContent = [...sharedLines, "Old ending", ""].join("\n");
    const afterContent = [...sharedLines, "New ending", ""].join("\n");
    fetchFileMock.mockResolvedValue({
      content: afterContent,
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
            "@@ -1206,1 +1206,1 @@",
            "-Old ending",
            "+New ending",
          ].join("\n")}
          documentContent={{
            before: {
              content: beforeContent,
              source: "index",
            },
            after: {
              content: afterContent,
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-large-markdown-deferred"
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

    expect(screen.getByLabelText("Markdown diff status")).toBeInTheDocument();
    expect(screen.queryByText("Patch preview")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Edit full document" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Render full document" })).toBeNull();
    const renderedMarkdownText =
      document.querySelector(".markdown-diff-change-scroll")?.textContent ?? "";
    expect(renderedMarkdownText).toContain("Shared line 1");
    expect(renderedMarkdownText).toContain("Shared line 1205");
    expect(renderedMarkdownText).toContain("New ending");
    expect(
      screen.queryByText("Preview reconstructed from the patch. Unchanged regions outside shown hunks are omitted."),
    ).not.toBeInTheDocument();
    expect(document.querySelector("[data-markdown-editable='true']")).toBeNull();

    await clickAndSettle(screen.getByRole("button", { name: "Edit full document" }));

    expect(screen.queryByRole("button", { name: "Edit full document" })).toBeNull();
    await waitFor(() => {
      expect(document.querySelector("[data-markdown-editable='true']")).not.toBeNull();
    });
  });

  it("defers editable full-document sections for low-line Markdown over the character cap", async () => {
    const longBody = "Wide paragraph content. ".repeat(6_100);
    const beforeContent = `# Base document\n\n${longBody}Old ending.\n`;
    const afterContent = `# Draft document\n\n${longBody}New ending.\n`;
    fetchFileMock.mockResolvedValue({
      content: afterContent,
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
            "@@ -1,3 +1,3 @@",
            "-# Base document",
            "+# Draft document",
          ].join("\n")}
          documentContent={{
            before: {
              content: beforeContent,
              source: "index",
            },
            after: {
              content: afterContent,
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-large-markdown-char-deferred"
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

    expect(screen.getByLabelText("Markdown diff status")).toBeInTheDocument();
    expect(document.querySelector("[data-markdown-editable='true']")).toBeNull();
    expect(
      document.querySelector(".markdown-diff-change-scroll")?.textContent ?? "",
    ).toContain("New ending.");
    // The adjacent large-Markdown case exercises the shared click-through
    // transition into editable mode. This character-cap case pins only its
    // distinct fallback preview and affordance so it does not duplicate the
    // timing-sensitive editor transition.
    expect(
      screen.getByRole("button", { name: "Edit full document" }),
    ).toBeInTheDocument();
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

    expect(screen.queryByText("Patch preview")).not.toBeInTheDocument();
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

  // Regression guard for "Silent CRLF→LF conversion on rendered-Markdown
  // save" in docs/bugs.md. Before the fix, a CRLF-on-disk document
  // edited through the rendered-Markdown path would be silently
  // rewritten as LF on the first commit: the commit handler
  // LF-normalized `sourceContent` for segment math and then wrote the
  // LF-normalized `nextDocumentContent` back into the edit buffer via
  // `setEditValueState`, and the next `handleSave` persisted that LF
  // version. The fix captures the original EOL style at the source-
  // content boundary and re-applies it after the segment math, so the
  // save sees CRLF going out as CRLF.
  it("preserves CRLF line endings when saving a rendered Markdown edit on a CRLF file", async () => {
    const crlfDiskContent =
      "Shared intro.\r\n# Draft document\r\nShared middle.\r\nReady to commit.\r\nShared outro.\r\n";
    const expectedSavedContent =
      "Shared intro.\r\n# Draft document\r\nShared middle.\r\nReady to ship.\r\nShared outro.\r\n";
    fetchFileMock.mockResolvedValue({
      content: crlfDiskContent,
      language: "markdown",
      path: "/repo/README.md",
    });
    const onSaveFile = vi.fn().mockResolvedValue({
      content: expectedSavedContent,
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
            " # Draft document",
            " Shared middle.",
            "-Committed text.",
            "+Ready to commit.",
            " Shared outro.",
          ].join("\n")}
          documentContent={{
            before: {
              content:
                "Shared intro.\r\n# Draft document\r\nShared middle.\r\nCommitted text.\r\nShared outro.\r\n",
              source: "index",
            },
            after: {
              content: crlfDiskContent,
              source: "worktree",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-markdown-crlf-preservation"
          filePath="/repo/README.md"
          gitSectionId="unstaged"
          language="markdown"
          sessionId="session-1"
          workspaceRoot="/repo"
          onOpenPath={() => {}}
          onSaveFile={onSaveFile}
          summary="Updated README (CRLF)"
        />,
      );
    });

    await waitFor(() => {
      expect(document.querySelectorAll(".markdown-diff-rendered-section-added").length).toBeGreaterThan(0);
    });

    const editableAddedSections = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    );
    const targetSection = Array.from(editableAddedSections).find((section) =>
      (section.textContent ?? "").includes("Ready to commit."),
    );
    expect(targetSection).toBeDefined();
    if (!targetSection) {
      return;
    }

    editRenderedMarkdownSection(targetSection, "<p>Ready to ship.</p>");
    fireEvent.blur(targetSection);

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    // The saved payload must preserve CRLF — no `\n` that isn't part
    // of a `\r\n`, and the expected full document reassembled with
    // CRLF separators reaches the save handler verbatim.
    expect(onSaveFile).toHaveBeenCalledTimes(1);
    const [, persistedContent] = onSaveFile.mock.calls[0];
    expect(persistedContent).toBe(expectedSavedContent);
    expect(persistedContent).toContain("\r\n");
    expect(persistedContent).not.toMatch(/(?<!\r)\n/);
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
