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

  it("adopts a successful save before reporting post-save rendered draft failure", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      contentHash: "sha256:base",
      language: "markdown",
      path: "/repo/README.md",
    });
    const firstSave = createDeferred<{
      content: string;
      contentHash: string;
      language: string;
      path: string;
    }>();
    const savedCapture: Array<{
      content: string;
      options?: { baseHash?: string | null; overwrite?: boolean };
      path: string;
    }> = [];
    const onSaveFile = vi
      .fn()
      .mockImplementationOnce(
        async (
          path: string,
          content: string,
          options?: { baseHash?: string | null; overwrite?: boolean },
        ) => {
          savedCapture.push({ content, options, path });
          return firstSave.promise;
        },
      )
      .mockImplementation(
        async (
          path: string,
          content: string,
          options?: { baseHash?: string | null; overwrite?: boolean },
        ) => {
          savedCapture.push({ content, options, path });
          return { content, contentHash: "sha256:second", language: "markdown", path };
        },
      );

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
          diffMessageId="diff-markdown-save-post-success-reject"
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
    const firstContent = "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n";
    const secondContent = "Shared intro.\n# Draft document\nShared middle.\nReady to launch.\nShared outro.\n";

    hasOverlappingMarkdownCommitRangesMock
      .mockReturnValueOnce(false)
      .mockReturnValueOnce(true);
    editRenderedMarkdownSection(addedSections[1], "<p>Ready to ship.</p>");
    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));
    expect(savedCapture[0]).toEqual({
      content: firstContent,
      options: { baseHash: "sha256:base", overwrite: undefined },
      path: "/repo/README.md",
    });

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
      await Promise.resolve();
    });

    await act(async () => {
      firstSave.resolve({
        content: firstContent,
        contentHash: "sha256:first",
        language: "markdown",
        path: "/repo/README.md",
      });
      await Promise.resolve();
    });

    expect(
      await screen.findByText(
        "Save failed: Rendered Markdown edit could not be applied because the document changed under that section. Review the latest diff and edit again.",
      ),
    ).toBeInTheDocument();

    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    await waitFor(() => {
      expect(savedCapture).toHaveLength(2);
    });
    expect(savedCapture[1]).toEqual({
      content: secondContent,
      options: { baseHash: "sha256:first", overwrite: undefined },
      path: "/repo/README.md",
    });
  });

  it("preserves code-mode edits when post-save rendered draft commit fails", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
      contentHash: "sha256:base",
      language: "markdown",
      path: "/repo/README.md",
    });
    const firstSave = createDeferred<{
      content: string;
      contentHash: string;
      language: string;
      path: string;
    }>();
    const onSaveFile = vi.fn().mockImplementationOnce(() => firstSave.promise);

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
          diffMessageId="diff-markdown-save-code-edit-preserved"
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

    const savedContent = "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n";
    const codeEditedContent =
      "Shared intro.\n# Draft document\nShared middle.\nReady from code mode.\nShared outro.\n";

    hasOverlappingMarkdownCommitRangesMock
      .mockReturnValueOnce(false)
      .mockReturnValueOnce(true);
    editRenderedMarkdownSection(addedSections[1], "<p>Ready to ship.</p>");
    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));
    await changeAndSettle(await screen.findByTestId("monaco-code-editor"), {
      target: { value: codeEditedContent },
    });
    await clickAndSettle(screen.getByRole("button", { name: "Rendered Markdown" }));

    const pendingSection = await waitFor(() => {
      const sections = document.querySelectorAll<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(sections.length).toBeGreaterThan(0);
      return sections[sections.length - 1];
    });
    editRenderedMarkdownSection(pendingSection, "<p>Ready from rendered mode.</p>");

    await act(async () => {
      firstSave.resolve({
        content: savedContent,
        contentHash: "sha256:first",
        language: "markdown",
        path: "/repo/README.md",
      });
      await Promise.resolve();
    });

    expect(
      await screen.findByText(
        "Save failed: Rendered Markdown edit could not be applied because the document changed under that section. Review the latest diff and edit again.",
      ),
    ).toBeInTheDocument();

    await clickAndSettle(screen.getByRole("button", { name: "Edit mode" }));
    expect(await screen.findByTestId("monaco-code-editor")).toHaveValue(codeEditedContent);
  });

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

  it("keeps rendered Markdown drafts active when documentContent refresh cannot commit them", async () => {
    fetchFileMock.mockResolvedValue({
      content: "# Title\n\nSection one original.\n",
      language: "markdown",
      path: "/repo/notes.md",
    });
    const onSaveFile = vi.fn();

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
      diffMessageId: "diff-markdown-document-content-rejected-draft",
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

    const section = await waitFor(() => {
      const candidate = document.querySelector<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(candidate).toBeTruthy();
      return candidate!;
    });
    editRenderedMarkdownSection(section, "<p>Section one local draft.</p>");
    hasOverlappingMarkdownCommitRangesMock.mockReturnValueOnce(true);

    rerender(
      <DiffPanel
        {...baseProps}
        documentContent={{
          ...baseProps.documentContent,
          before: {
            content: "# Title\n\nExternal intro.\n\nSection one base.\n",
            source: "index" as const,
          },
          after: {
            content: "# Title\n\nExternal intro.\n\nSection one original.\n",
            source: "worktree" as const,
          },
        }}
        workspaceFilesChangedEvent={null}
      />,
    );

    expect(
      await screen.findByText(
        "Save failed: Rendered Markdown edit could not be applied because the document changed under that section. Review the latest diff and edit again.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save Markdown" })).toBeEnabled();
    expect(onSaveFile).not.toHaveBeenCalled();
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

  it("does not refresh from a watcher event when rendered Markdown drafts cannot commit", async () => {
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
      diffMessageId: "diff-markdown-watch-rejected-draft",
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

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledTimes(1);
    });
    const section = await waitFor(() => {
      const candidate = document.querySelector<HTMLElement>(
        ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
      );
      expect(candidate).toBeTruthy();
      return candidate!;
    });
    editRenderedMarkdownSection(section, "<p>Section one local draft.</p>");
    hasOverlappingMarkdownCommitRangesMock.mockReturnValueOnce(true);

    rerender(
      <DiffPanel
        {...baseProps}
        workspaceFilesChangedEvent={{
          revision: 9,
          changes: [{ path: "/repo/notes.md", kind: "modified" }],
        }}
      />,
    );

    expect(
      await screen.findByText(
        "Save failed: Rendered Markdown edit could not be applied because the document changed under that section. Review the latest diff and edit again.",
      ),
    ).toBeInTheDocument();
    expect(fetchFileMock).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Save Markdown" })).toBeEnabled();
  });

  // Regression: rendered-mode edit handlers used `markdownPreview.after.content`
  // for their baseline, but the displayed segments were computed from the
  // dirty `editValue` buffer when the user had pending code-mode edits. The
  // offsets therefore pointed into a stale content string and silently
  // discarded the code-mode changes. The fix snapshots the segment source
  // content alongside the segments and passes it explicitly to the handlers.
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

  // Regression guard for the new `commitRenderedMarkdownDrafts`
  // return-boolean plumbing in
  // `handleApplyDiffEditsToDiskVersion`. Before the fix the
  // handler called `flushSync(() => commitRenderedMarkdownDrafts())`
  // and discarded the result; on a failing commit the rebase
  // would silently continue. The fix captures the boolean and
  // short-circuits with an explicit `externalFileNotice`.
  //
  // What this test PINS (limited but useful):
  //   - The empty-commits early-return in
  //     `commitRenderedMarkdownDrafts` returns `true` (not
  //     `undefined`/`false`), so `handleApplyDiffEditsToDiskVersion`
  //     does NOT spuriously short-circuit on the conflict-notice
  //     branch when there's nothing to flush. The full
  //     apply-to-disk-version flow continues through `fetchFile`
  //     and the rebase to its success notice.
  //   - A regression that inverted the boolean polarity, or that
  //     changed the empty-commits path to `return` (undefined),
  //     would cause this test to fail: either the
  //     "Resolve rendered Markdown conflicts..." notice would
  //     appear, or the success notice would never show.
  //
  // What this test does NOT pin (tracked as P2 in docs/bugs.md):
  //   - The `handleRenderedMarkdownSectionCommits(commits) → true`
  //     branch of `commitRenderedMarkdownDrafts`. `handleSave`
  //     synchronously commits drafts BEFORE `onSaveFile` rejects,
  //     so by the time this test clicks apply-to-disk-version the
  //     committers return `null` and the flushSync takes the
  //     `commits.length === 0` path. Re-editing a section AFTER
  //     the failed save to produce a fresh dirty draft doesn't
  //     reliably land on the success branch either — the
  //     post-first-commit source buffer already advanced past
  //     the re-edited segment's original markdown, so the
  //     resolver fails and the commit returns false.
  //   - The conflict-short-circuit path. The P2 task enumerates
  //     two alternative approaches: extracting
  //     `handleRenderedMarkdownSectionCommits` into a pure helper,
  //     or mocking `hasOverlappingMarkdownCommitRanges` via
  //     `vi.mock` to force a deterministic failure.
  it("keeps apply-to-disk-version flowing when `commitRenderedMarkdownDrafts` has nothing to flush", async () => {
    fetchFileMock
      .mockResolvedValueOnce({
        content: "Shared intro.\n# Draft document\nShared middle.\nReady to commit.\nShared outro.\n",
        contentHash: "sha256:base",
        language: "markdown",
        path: "/repo/README.md",
      })
      .mockResolvedValueOnce({
        content: "Shared intro.\n# Draft document\nShared middle.\nReady to ship.\nShared outro.\n",
        contentHash: "sha256:disk",
        language: "markdown",
        path: "/repo/README.md",
      });
    const onSaveFile = vi
      .fn()
      .mockRejectedValueOnce(new Error("file changed on disk before save"));

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
          diffMessageId="diff-apply-disk-rendered-markdown"
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
      expect(
        document.querySelectorAll(".markdown-diff-rendered-section-added").length,
      ).toBe(2);
    });

    const addedSections = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    );
    const readyToCommitSection = Array.from(addedSections).find((section) =>
      (section.textContent ?? "").includes("Ready to commit."),
    );
    expect(readyToCommitSection).toBeDefined();
    if (!readyToCommitSection) {
      return;
    }

    // Edit the section, Save it (which commits the draft
    // synchronously via `handleSave`'s own
    // `commitRenderedMarkdownDrafts()` call and then rejects on
    // the network), arm the apply-to-disk-version button.
    editRenderedMarkdownSection(
      readyToCommitSection,
      "<p>Ready to ship.</p>",
    );
    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    const applyButton = await screen.findByRole("button", {
      name: "Apply my edits to disk version",
    });

    // At this point, `hasUncommittedUserEditRef` on every
    // committer is false (Save's internal flush cleared them).
    // Clicking apply-to-disk-version hits the `commits.length ===
    // 0` empty-path in `commitRenderedMarkdownDrafts`, which
    // must return `true` so
    // `handleApplyDiffEditsToDiskVersion` does NOT set the
    // conflict notice and DOES proceed to `fetchFile` + rebase.
    await clickAndSettle(applyButton);

    // Pin the rebase fetch by path + scope, not by count delta,
    // so a future watcher-tick refactor doesn't leak into the
    // assertion.
    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenLastCalledWith(
        "/repo/README.md",
        expect.objectContaining({ sessionId: "session-1" }),
      );
    });
    expect(
      await screen.findByText("Your diff edits were applied on top of the disk version."),
    ).toBeInTheDocument();
    // Negative control: the conflict-short-circuit notice must
    // NOT appear when the empty-commits path correctly returns
    // `true`. A regression that flipped the polarity to `false`
    // on empty commits would trip this.
    expect(
      screen.queryByText(
        "Resolve rendered Markdown conflicts before applying edits to the disk version.",
      ),
    ).not.toBeInTheDocument();
  });

  // Regression guard for the save-error-over-gated fix in
  // `DiffPanel.tsx`. Previously the "Save failed: <reason>"
  // diagnostic was gated on `!externalFileNotice &&
  // !diffEditConflictOnDisk`, which suppressed the diagnostic
  // whenever ANY `externalFileNotice` was visible — including
  // purely informational notices like "Rendered Markdown edits
  // will save this document to the worktree file." (set when
  // editing a rendered-Markdown diff whose `after.source !==
  // "worktree"`). A save failure while that informational notice
  // was visible produced a "Save failed" pill with no diagnostic
  // — the exact regression the diagnostic was added to prevent.
  //
  // The fix narrows the gate to `!diffEditConflictOnDisk` only.
  // The conflict path still renders its own recovery UI (with
  // "Apply my edits to disk version" / "Save anyway" / "Reload
  // from disk" buttons) in place of the raw diagnostic; the
  // informational notice can now coexist with the diagnostic.
  it("surfaces the save-error diagnostic when an informational externalFileNotice is visible", async () => {
    fetchFileMock.mockResolvedValue({
      content: "Shared intro.\n# Staged document\nShared middle.\nReady to commit.\nShared outro.\n",
      contentHash: "sha256:base",
      language: "markdown",
      path: "/repo/README.md",
    });
    const onSaveFile = vi
      .fn()
      // A non-stale error: `isStaleFileSaveError` returns false,
      // so the catch branch takes only `setSaveError(message)`
      // and does NOT set `externalFileNotice` / flip
      // `diffEditConflictOnDisk`. The informational notice set
      // during the keystroke handler stays visible.
      .mockRejectedValue(new Error("permission denied"));

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
              source: "index",
            },
            // `source: "index"` (not "worktree") triggers the
            // informational notice in the keystroke handler at
            // `handleRenderedMarkdownSectionDraftChange`. Pairing
            // with `gitSectionId: "unstaged"` keeps the diff
            // editable (`isStagedMarkdownDiff === false`) —
            // staged Markdown diffs are read-only so the notice
            // handler would never run against them.
            after: {
              content: "Shared intro.\n# Staged document\nShared middle.\nReady to commit.\nShared outro.\n",
              source: "index",
            },
            canEdit: true,
            isCompleteDocument: true,
          }}
          diffMessageId="diff-save-error-informational-notice"
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
      expect(
        document.querySelectorAll(".markdown-diff-rendered-section-added").length,
      ).toBeGreaterThan(0);
    });

    const addedSections = document.querySelectorAll<HTMLElement>(
      ".markdown-diff-rendered-section-added [data-markdown-editable='true']",
    );
    const readyToCommitSection = Array.from(addedSections).find((section) =>
      (section.textContent ?? "").includes("Ready to commit."),
    );
    expect(readyToCommitSection).toBeDefined();
    if (!readyToCommitSection) {
      return;
    }

    // Editing arms the informational notice via the keystroke
    // handler. The notice is set to "Rendered Markdown edits
    // will save this document to the worktree file." because
    // `after.source !== "worktree"`.
    editRenderedMarkdownSection(readyToCommitSection, "<p>Ready to ship.</p>");
    expect(
      screen.getByText("Rendered Markdown edits will save this document to the worktree file."),
    ).toBeInTheDocument();

    // Save rejects with a non-stale error → `setSaveError`
    // runs, informational notice is NOT cleared.
    await clickAndSettle(screen.getByRole("button", { name: "Save Markdown" }));

    // Pill label.
    expect(await screen.findByText("Save failed")).toBeInTheDocument();
    // PRIMARY ASSERTION: the diagnostic text is visible even
    // though the informational notice is also visible. Reverting
    // the fix (restoring `!externalFileNotice` in the gate) makes
    // this assertion fail — the diagnostic would be suppressed.
    expect(screen.getByText(/Save failed: permission denied/i)).toBeInTheDocument();
    // Secondary: the informational notice stays visible alongside
    // the diagnostic. The two do not compete — they stack.
    expect(
      screen.getByText("Rendered Markdown edits will save this document to the worktree file."),
    ).toBeInTheDocument();
    // Negative control: the stale-save recovery UI must NOT
    // appear. `permission denied` is not a stale-file error, so
    // `diffEditConflictOnDisk` stays false and the recovery
    // buttons do not render.
    expect(
      screen.queryByRole("button", { name: "Apply my edits to disk version" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Save anyway" }),
    ).not.toBeInTheDocument();
    // All three recovery buttons render under the same
    // `diffEditConflictOnDisk && latestFile.status === "ready"`
    // gate — assert all three so the negative control matches
    // the regression comment's "Apply / Save anyway / Reload
    // from disk" enumeration.
    expect(
      screen.queryByRole("button", { name: "Reload from disk" }),
    ).not.toBeInTheDocument();
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


});
