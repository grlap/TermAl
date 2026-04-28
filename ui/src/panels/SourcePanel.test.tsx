import { act, createEvent, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { copyTextToClipboard } from "../clipboard";
import {
  SourcePanel,
  type SourceFileState,
} from "./SourcePanel";
import { rebaseContentOntoDisk } from "./content-rebase";
import { EditableRenderedMarkdownSection } from "./markdown-diff-change-section";
import type { MarkdownDiffDocumentSegment } from "./markdown-diff-segments";

vi.mock("../clipboard", () => ({
  copyTextToClipboard: vi.fn(),
}));

// Mock Monaco as a textarea. `inlineZones` is surfaced as a data
// attribute (count + first-zone line) so tests can assert zones were
// passed without spinning up a real editor with view-zone machinery.
vi.mock("../MonacoCodeEditor", () => ({
  MonacoCodeEditor: ({
    ariaLabel,
    inlineZones,
    onChange,
    onSave,
    value,
  }: {
    ariaLabel: string;
    inlineZones?: Array<{ id: string; afterLineNumber: number }>;
    onChange: (nextValue: string) => void;
    onSave?: () => void;
    value: string;
  }) => (
    <textarea
      aria-label={ariaLabel}
      data-inline-zone-count={inlineZones?.length ?? 0}
      data-inline-zone-first-after-line={inlineZones?.[0]?.afterLineNumber ?? ""}
      data-inline-zone-ids={inlineZones?.map((zone) => zone.id).join(",") ?? ""}
      onChange={(event) => onChange(event.currentTarget.value)}
      onKeyDown={(event) => {
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
          event.preventDefault();
          onSave?.();
        }
      }}
      value={value}
    />
  ),
}));

vi.mock("../MonacoDiffEditor", () => ({
  MonacoDiffEditor: ({
    ariaLabel,
    modifiedValue,
    originalValue,
  }: {
    ariaLabel: string;
    modifiedValue: string;
    originalValue: string;
  }) => (
    <div aria-label={ariaLabel} role="region">
      <pre data-testid="diff-original">{originalValue}</pre>
      <pre data-testid="diff-modified">{modifiedValue}</pre>
    </div>
  ),
}));

const copyTextToClipboardMock = vi.mocked(copyTextToClipboard);

function createDeferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
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

function selectRenderedMarkdownSectionContents(section: HTMLElement) {
  act(() => {
    section.focus();
    const markdownRoot = section.querySelector<HTMLElement>(".markdown-copy") ?? section;
    const range = document.createRange();
    range.selectNodeContents(markdownRoot);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  });
}

function selectRenderedMarkdownNode(section: HTMLElement, node: Node) {
  act(() => {
    section.focus();
    const range = document.createRange();
    range.selectNode(node);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  });
}

const readyFileState: SourceFileState = {
  status: "ready",
  path: "src/main.rs",
  content: "fn main() {}\n",
  contentHash: "sha256:base",
  mtimeMs: 1,
  sizeBytes: 13,
  staleOnDisk: false,
  externalContentHash: null,
  externalMtimeMs: null,
  externalSizeBytes: null,
  error: null,
  language: "rust",
};

const editorAppearance = "light";

describe("SourcePanel", () => {
  beforeEach(() => {
    copyTextToClipboardMock.mockReset();
    copyTextToClipboardMock.mockResolvedValue(undefined);
  });

  it("renders a read-only file label with an inline loading spinner and copy action", async () => {
    const fileState: SourceFileState = {
      status: "loading",
      path: "C:/repo/docs/model-switching.md",
      content: "",
      contentHash: null,
      mtimeMs: null,
      sizeBytes: null,
      staleOnDisk: false,
      externalContentHash: null,
      externalMtimeMs: null,
      externalSizeBytes: null,
      error: null,
      language: null,
    };

    const { container } = render(
      <SourcePanel
        editorAppearance="light"
        editorFontSizePx={14}
        fileState={fileState}
        sourcePath={fileState.path}
        onSaveFile={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.getByText(fileState.path)).toBeInTheDocument();
    expect(
      screen.getByRole("status", { name: "Loading source file" }),
    ).toBeInTheDocument();
    expect(container.querySelector(".source-path-loading-spinner")).not.toBeNull();
    expect(screen.queryByText("Loading file")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Copy path" }));

    await waitFor(() => {
      expect(copyTextToClipboardMock).toHaveBeenCalledWith(fileState.path);
    });
  });

  it("shows stale disk changes and reloads on request", async () => {
    const onReloadFile = vi.fn().mockResolvedValue(undefined);

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          staleOnDisk: true,
          externalContentHash: "sha256:disk",
        }}
        sourcePath="src/main.rs"
        onReloadFile={onReloadFile}
        onSaveFile={vi.fn()}
      />,
    );

    expect(screen.getByText("File changed on disk")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Reload from disk" }));

    await waitFor(() => {
      expect(onReloadFile).toHaveBeenCalledWith("src/main.rs");
    });
  });

  it("reports dirty state when the editor buffer changes", async () => {
    const onDirtyChange = vi.fn();

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={readyFileState}
        sourcePath="src/main.rs"
        onDirtyChange={onDirtyChange}
        onSaveFile={vi.fn()}
      />,
    );

    await waitFor(() => {
      expect(onDirtyChange).toHaveBeenCalledWith(false);
    });
    onDirtyChange.mockClear();

    fireEvent.change(
      await screen.findByLabelText("Source editor for src/main.rs"),
      {
        target: { value: "fn main() { println!(\"changed\"); }\n" },
      },
    );

    await waitFor(() => {
      expect(onDirtyChange).toHaveBeenLastCalledWith(true);
    });
    expect(onDirtyChange).toHaveBeenCalledTimes(1);
  });

  it("renders Markdown preview from the unsaved editor buffer", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "# Original\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.change(
      await screen.findByLabelText("Source editor for /repo/docs/readme.md"),
      {
        target: { value: "# Changed Title\n\n- [x] Done\n" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));

    expect(await screen.findByRole("heading", { name: "Changed Title" })).toBeInTheDocument();
    expect(screen.getByText("Done")).toBeInTheDocument();
  });

  it("edits Markdown source from rendered Preview and saves the shared buffer", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={onSaveFile}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    editRenderedMarkdownSection(renderedSection, "<p>Updated paragraph.</p>");

    expect(await screen.findByText("Unsaved changes")).toBeInTheDocument();

    fireEvent.keyDown(renderedSection, {
      key: "s",
      code: "KeyS",
      ctrlKey: true,
    });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "/repo/docs/readme.md",
        "Updated paragraph.\n",
        undefined,
      );
    });
  });

  it("sanitizes dropped rendered Markdown HTML before saving", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={onSaveFile}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    selectRenderedMarkdownSectionContents(renderedSection);
    fireEvent.drop(renderedSection, {
      dataTransfer: {
        getData: vi.fn((type: string) => {
          if (type === "text/html") {
            return [
              '<a href="javascript:alert(1)" onclick="alert(2)">Dropped link</a>',
              '<img src="https://example.com/tracker.png" alt="tracker">',
              "<p>Safe paragraph</p>",
            ].join("");
          }
          if (type === "text/plain") {
            return "Dropped link\nSafe paragraph";
          }
          return "";
        }),
      },
    });

    expect(renderedSection.querySelector("img")).toBeNull();
    expect(
      Array.from(renderedSection.querySelectorAll<HTMLElement>("*")).some((element) =>
        Array.from(element.attributes).some((attribute) =>
          attribute.name.toLowerCase().startsWith("on"),
        ),
      ),
    ).toBe(false);
    expect(
      Array.from(renderedSection.querySelectorAll<HTMLAnchorElement>("a[href]")).some((link) =>
        (link.getAttribute("href") ?? "").trim().toLowerCase().startsWith("javascript:"),
      ),
    ).toBe(false);

    fireEvent.keyDown(renderedSection, {
      key: "s",
      code: "KeyS",
      ctrlKey: true,
    });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "/repo/docs/readme.md",
        "Dropped link\n\nSafe paragraph\n",
        undefined,
      );
    });
    const savedContent = onSaveFile.mock.calls[0]?.[1] as string;
    expect(savedContent).not.toContain("javascript:");
    expect(savedContent).not.toContain("tracker.png");
  });

  it("prevents the browser default for empty rendered Markdown drops", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });
    const dropEvent = createEvent.drop(renderedSection, {
      dataTransfer: {
        getData: vi.fn(() => ""),
      },
    });

    fireEvent(renderedSection, dropEvent);

    expect(dropEvent.defaultPrevented).toBe(true);
    expect(renderedSection).toHaveTextContent("Original paragraph.");
  });

  it("uses text/uri-list as the rendered Markdown drop fallback", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={onSaveFile}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    selectRenderedMarkdownSectionContents(renderedSection);
    fireEvent.drop(renderedSection, {
      dataTransfer: {
        getData: vi.fn((type: string) => {
          if (type === "text/uri-list") {
            return "# dragged link\nhttps://example.com/docs/readme\n";
          }
          return "";
        }),
      },
    });
    expect(renderedSection).toHaveTextContent("https://example.com/docs/readme");

    fireEvent.keyDown(renderedSection, {
      key: "s",
      code: "KeyS",
      ctrlKey: true,
    });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "/repo/docs/readme.md",
        "https://example.com/docs/readme\n",
        undefined,
      );
    });
  });

  it("inserts rendered Markdown drops at the pointer caret instead of the stale selection", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);
    const documentWithCaretRange = document as Document & {
      caretRangeFromPoint?: (x: number, y: number) => Range | null;
    };
    const originalCaretRangeFromPoint = Object.getOwnPropertyDescriptor(
      document,
      "caretRangeFromPoint",
    );

    try {
      render(
        <SourcePanel
          editorAppearance={editorAppearance}
          editorFontSizePx={14}
          fileState={{
            ...readyFileState,
            path: "/repo/docs/readme.md",
            content: "Original paragraph.\n",
            language: "markdown",
          }}
          sourcePath="/repo/docs/readme.md"
          workspaceRoot="/repo"
          onSaveFile={onSaveFile}
        />,
      );

      fireEvent.click(screen.getByRole("button", { name: "Preview" }));
      const renderedSection = await waitFor(() => {
        const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
        expect(section).not.toBeNull();
        return section!;
      });
      const markdownRoot = renderedSection.querySelector<HTMLElement>(".markdown-copy");
      const paragraphText = markdownRoot?.querySelector("p")?.firstChild;
      expect(paragraphText?.nodeType).toBe(Node.TEXT_NODE);

      const staleRange = document.createRange();
      staleRange.selectNodeContents(markdownRoot!);
      staleRange.collapse(false);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(staleRange);

      const dropRange = document.createRange();
      dropRange.setStart(paragraphText!, "Original ".length);
      dropRange.collapse(true);
      Object.defineProperty(documentWithCaretRange, "caretRangeFromPoint", {
        configurable: true,
        value: vi.fn(() => dropRange),
      });

      fireEvent.drop(renderedSection, {
        clientX: 12,
        clientY: 34,
        dataTransfer: {
          getData: vi.fn((type: string) => (type === "text/plain" ? "dropped " : "")),
        },
      });

      expect(renderedSection).toHaveTextContent("Original dropped paragraph.");

      fireEvent.keyDown(renderedSection, {
        key: "s",
        code: "KeyS",
        ctrlKey: true,
      });

      await waitFor(() => {
        expect(onSaveFile).toHaveBeenCalledWith(
          "/repo/docs/readme.md",
          "Original dropped paragraph.\n",
          undefined,
        );
      });
    } finally {
      if (originalCaretRangeFromPoint) {
        Object.defineProperty(document, "caretRangeFromPoint", originalCaretRangeFromPoint);
      } else {
        Reflect.deleteProperty(documentWithCaretRange, "caretRangeFromPoint");
      }
    }
  });

  it("copies editable rendered Markdown as text/plain Markdown", async () => {
    const setClipboardData = vi.fn();

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    selectRenderedMarkdownSectionContents(renderedSection);
    const copyEvent = createEvent.copy(renderedSection, {
      clipboardData: {
        setData: setClipboardData,
      },
    });
    fireEvent(renderedSection, copyEvent);

    expect(copyEvent.defaultPrevented).toBe(true);
    expect(setClipboardData).toHaveBeenCalledWith(
      "text/plain",
      "Original paragraph.",
    );
  });

  it("copies partial rendered Markdown text selections as text/plain Markdown", async () => {
    const setClipboardData = vi.fn();

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Hello world!\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });
    const textNode = await waitFor(() => {
      const node = renderedSection.querySelector("p")?.firstChild;
      expect(node?.nodeType).toBe(Node.TEXT_NODE);
      return node!;
    });

    act(() => {
      renderedSection.focus();
      const range = document.createRange();
      range.setStart(textNode, 6);
      range.setEnd(textNode, 11);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
    });
    const copyEvent = createEvent.copy(renderedSection, {
      clipboardData: {
        setData: setClipboardData,
      },
    });
    fireEvent(renderedSection, copyEvent);

    expect(copyEvent.defaultPrevented).toBe(true);
    expect(setClipboardData).toHaveBeenCalledWith("text/plain", "world");
    expect(setClipboardData).not.toHaveBeenCalledWith(
      "text/plain",
      "Hello world!",
    );
  });

  it("does not copy a whole rendered Markdown segment for an unserialized partial selection", async () => {
    const setClipboardData = vi.fn();

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Before\n\n![diagram](https://example.com/diagram.png)\n\nAfter\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });
    const image = await waitFor(() => {
      const candidate = renderedSection.querySelector("img");
      expect(candidate).not.toBeNull();
      return candidate!;
    });

    selectRenderedMarkdownNode(renderedSection, image);
    const copyEvent = createEvent.copy(renderedSection, {
      clipboardData: {
        setData: setClipboardData,
      },
    });
    fireEvent(renderedSection, copyEvent);

    expect(copyEvent.defaultPrevented).toBe(true);
    expect(setClipboardData).toHaveBeenCalledWith("text/plain", "");
    expect(setClipboardData).not.toHaveBeenCalledWith(
      "text/plain",
      "Before\n\n![diagram](https://example.com/diagram.png)\n\nAfter\n",
    );
  });

  it("cuts editable rendered Markdown as text/plain Markdown and commits the deletion", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);
    const setClipboardData = vi.fn();

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={onSaveFile}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    selectRenderedMarkdownSectionContents(renderedSection);
    const cutEvent = createEvent.cut(renderedSection, {
      clipboardData: {
        setData: setClipboardData,
      },
    });
    fireEvent(renderedSection, cutEvent);

    expect(cutEvent.defaultPrevented).toBe(true);
    expect(setClipboardData).toHaveBeenCalledWith(
      "text/plain",
      "Original paragraph.",
    );

    fireEvent.keyDown(renderedSection, {
      key: "s",
      ctrlKey: true,
    });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "/repo/docs/readme.md",
        "\n",
        undefined,
      );
    });
  });

  it("does not cut a whole rendered Markdown segment for an unserialized partial selection", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);
    const setClipboardData = vi.fn();

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Before\n\n![diagram](https://example.com/diagram.png)\n\nAfter\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={onSaveFile}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });
    const image = await waitFor(() => {
      const candidate = renderedSection.querySelector("img");
      expect(candidate).not.toBeNull();
      return candidate!;
    });

    selectRenderedMarkdownNode(renderedSection, image);
    const cutEvent = createEvent.cut(renderedSection, {
      clipboardData: {
        setData: setClipboardData,
      },
    });
    fireEvent(renderedSection, cutEvent);

    expect(cutEvent.defaultPrevented).toBe(true);
    expect(setClipboardData).toHaveBeenCalledWith("text/plain", "");
    expect(setClipboardData).not.toHaveBeenCalledWith(
      "text/plain",
      "Before\n\n![diagram](https://example.com/diagram.png)\n\nAfter\n",
    );
  });

  it("commits rendered Markdown split edits back into the code editor buffer", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    const editor = await screen.findByLabelText("Source editor for /repo/docs/readme.md");
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    editRenderedMarkdownSection(renderedSection, "<p>Split paragraph.</p>");
    fireEvent.blur(renderedSection);

    await waitFor(() => {
      expect(editor).toHaveValue("Split paragraph.\n");
    });
  });

  it("can clear an accepted rendered draft without remounting the editable DOM", async () => {
    const markdown = "Original paragraph.\n";
    const segment: MarkdownDiffDocumentSegment = {
      afterEndOffset: markdown.length,
      afterStartOffset: 0,
      id: "segment-1",
      isInAfterDocument: true,
      kind: "normal",
      markdown,
      newStart: 1,
      oldStart: 1,
    };
    const onCommitSectionDraft = vi.fn((commit) => {
      commit.onApplied?.({ resetRenderedContent: false });
      return true;
    });
    const onDraftChange = vi.fn();

    render(
      <EditableRenderedMarkdownSection
        allowReadOnlyCaret={false}
        allowCurrentSegmentFallback={false}
        appearance={editorAppearance}
        canEdit
        className="markdown-diff-rendered-section-body"
        documentPath="/repo/docs/readme.md"
        editableAriaLabel="Edit rendered Markdown preview for /repo/docs/readme.md"
        onCommitDrafts={() => true}
        onCommitSectionDraft={onCommitSectionDraft}
        onDraftChange={onDraftChange}
        onOpenSourceLink={vi.fn()}
        onReadOnlyMutation={vi.fn()}
        onRegisterCommitter={() => vi.fn()}
        onSave={vi.fn()}
        segment={segment}
        sourceContent={markdown}
        workspaceRoot="/repo"
      />,
    );

    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });
    const renderedContent = renderedSection.querySelector<HTMLElement>(
      ".markdown-diff-rendered-section-content",
    );
    expect(renderedContent).not.toBeNull();
    expect(
      screen.getByRole("textbox", {
        name: "Edit rendered Markdown preview for /repo/docs/readme.md",
      }),
    ).toBe(renderedSection);
    expect(renderedSection).toHaveAttribute("aria-multiline", "true");

    editRenderedMarkdownSection(renderedSection, "<p>Edited paragraph.</p>");
    fireEvent.blur(renderedSection);

    expect(onCommitSectionDraft).toHaveBeenCalledTimes(1);
    expect(onDraftChange).toHaveBeenLastCalledWith(segment, segment.markdown);
    expect(
      renderedSection.querySelector(".markdown-diff-rendered-section-content"),
    ).toBe(renderedContent);
  });

  it("keeps rendered Markdown committer registration stable across prop rerenders", async () => {
    const markdown = "Original paragraph.\n";
    const segment: MarkdownDiffDocumentSegment = {
      afterEndOffset: markdown.length,
      afterStartOffset: 0,
      id: "segment-1",
      isInAfterDocument: true,
      kind: "normal",
      markdown,
      newStart: 1,
      oldStart: 1,
    };
    const unregisterCommitter = vi.fn();
    const onRegisterCommitter = vi.fn(() => unregisterCommitter);

    const renderSection = (sourceContent: string) => (
      <EditableRenderedMarkdownSection
        allowReadOnlyCaret={false}
        allowCurrentSegmentFallback={false}
        appearance={editorAppearance}
        canEdit
        className="markdown-diff-rendered-section-body"
        documentPath="/repo/docs/readme.md"
        editableAriaLabel="Edit rendered Markdown preview for /repo/docs/readme.md"
        onCommitDrafts={() => true}
        onCommitSectionDraft={() => true}
        onDraftChange={vi.fn()}
        onOpenSourceLink={vi.fn()}
        onReadOnlyMutation={vi.fn()}
        onRegisterCommitter={onRegisterCommitter}
        onSave={vi.fn()}
        segment={segment}
        sourceContent={sourceContent}
        workspaceRoot="/repo"
      />
    );

    const { rerender } = render(renderSection(markdown));

    await waitFor(() => {
      expect(onRegisterCommitter).toHaveBeenCalledTimes(1);
    });

    rerender(renderSection(`${markdown}\nExternal context.`));

    expect(onRegisterCommitter).toHaveBeenCalledTimes(1);
    expect(unregisterCommitter).not.toHaveBeenCalled();
  });

  it("commits rendered Markdown edits before switching document modes", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    editRenderedMarkdownSection(renderedSection, "<p>Mode-switched paragraph.</p>");
    fireEvent.click(screen.getByRole("button", { name: "Code" }));

    const editor = await screen.findByLabelText("Source editor for /repo/docs/readme.md");
    expect(editor).toHaveValue("Mode-switched paragraph.\n");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
  });

  it("keeps the current Markdown mode when switching cannot apply a rendered draft", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    const editor = await screen.findByLabelText("Source editor for /repo/docs/readme.md");
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    editRenderedMarkdownSection(renderedSection, "<p>Rendered paragraph.</p>");
    fireEvent.change(editor, {
      target: { value: "Code pane edit.\n" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));

    expect(
      await screen.findByText(/Rendered Markdown edit could not be applied/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Source editor for /repo/docs/readme.md")).toBeInTheDocument();
    expect(renderedSection).toHaveTextContent("Rendered paragraph.");
  });

  it("keeps rendered Markdown mounted while typing in the split code pane", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    const editor = await screen.findByLabelText("Source editor for /repo/docs/readme.md");
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });
    const renderedContent = renderedSection.querySelector<HTMLElement>(
      ".markdown-diff-rendered-section-content",
    );
    expect(renderedContent).not.toBeNull();

    fireEvent.change(editor, {
      target: { value: "Original paragraph.\n\nSecond paragraph.\n" },
    });

    await waitFor(() => {
      expect(renderedSection).toHaveTextContent("Second paragraph.");
    });
    expect(
      renderedSection.querySelector(".markdown-diff-rendered-section-content"),
    ).toBe(renderedContent);
  });

  it("rejects stale rendered split drafts instead of overwriting code-pane edits", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={onSaveFile}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    const editor = await screen.findByLabelText("Source editor for /repo/docs/readme.md");
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    editRenderedMarkdownSection(renderedSection, "<p>Rendered paragraph.</p>");
    fireEvent.change(editor, {
      target: { value: "Code pane edit.\n" },
    });
    fireEvent.blur(renderedSection);

    expect(
      await screen.findByText(/Rendered Markdown edit could not be applied/),
    ).toBeInTheDocument();
    expect(editor).toHaveValue("Code pane edit.\n");
    expect(renderedSection).toHaveTextContent("Rendered paragraph.");
    expect(onSaveFile).not.toHaveBeenCalled();
  });

  it("adopts a successful save while preserving failed post-save rendered drafts", async () => {
    const savedFile = createDeferred<SourceFileState>();
    const onSaveFile = vi.fn().mockReturnValue(savedFile.promise);
    const onAdoptFileState = vi.fn();
    const savedFileState: SourceFileState = {
      status: "ready",
      path: "/repo/docs/readme.md",
      content: "Rendered paragraph.\n",
      contentHash: "sha256:saved",
      error: null,
      language: "markdown",
      sizeBytes: "Rendered paragraph.\n".length,
    };

    function Harness() {
      const [currentFileState, setCurrentFileState] = useState<SourceFileState>({
        ...readyFileState,
        path: "/repo/docs/readme.md",
        content: "Original paragraph.\n",
        language: "markdown",
      });
      return (
        <SourcePanel
          editorAppearance={editorAppearance}
          editorFontSizePx={14}
          fileState={currentFileState}
          sourcePath="/repo/docs/readme.md"
          workspaceRoot="/repo"
          onAdoptFileState={(nextFileState) => {
            onAdoptFileState(nextFileState);
            setCurrentFileState(nextFileState);
          }}
          onSaveFile={onSaveFile}
        />
      );
    }

    render(<Harness />);

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    const editor = await screen.findByLabelText("Source editor for /repo/docs/readme.md");
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    editRenderedMarkdownSection(renderedSection, "<p>Rendered paragraph.</p>");
    fireEvent.keyDown(editor, {
      key: "s",
      code: "KeyS",
      ctrlKey: true,
    });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "/repo/docs/readme.md",
        "Rendered paragraph.\n",
        undefined,
      );
    });

    editRenderedMarkdownSection(renderedSection, "<p>Post-save rendered draft.</p>");
    fireEvent.change(editor, {
      target: { value: "Code pane edit.\n" },
    });

    await act(async () => {
      savedFile.resolve(savedFileState);
      await savedFile.promise;
    });

    await waitFor(() => {
      expect(onAdoptFileState).toHaveBeenCalledWith(savedFileState);
      expect(screen.getByLabelText("Source editor for /repo/docs/readme.md")).toHaveValue(
        "Code pane edit.\n",
      );
    });
    const actionFailureNotice = screen
      .getByText("Action failed", { selector: ".card-label" })
      .closest("article");
    expect(actionFailureNotice).not.toBeNull();
    expect(actionFailureNotice!).toHaveTextContent(
      "Rendered Markdown edit could not be applied because the document changed under that section. Review the latest file and edit again.",
    );
  });

  it("does not save when Ctrl+S cannot apply a rendered Markdown draft", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "Original paragraph.\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={onSaveFile}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    const editor = await screen.findByLabelText("Source editor for /repo/docs/readme.md");
    const renderedSection = await waitFor(() => {
      const section = document.querySelector<HTMLElement>("[data-markdown-editable='true']");
      expect(section).not.toBeNull();
      return section!;
    });

    editRenderedMarkdownSection(renderedSection, "<p>Rendered paragraph.</p>");
    fireEvent.change(editor, {
      target: { value: "Code pane edit.\n" },
    });
    fireEvent.keyDown(renderedSection, {
      key: "s",
      code: "KeyS",
      ctrlKey: true,
    });

    expect(
      await screen.findByText(/Rendered Markdown edit could not be applied/),
    ).toBeInTheDocument();
    expect(editor).toHaveValue("Code pane edit.\n");
    expect(renderedSection).toHaveTextContent("Rendered paragraph.");
    expect(onSaveFile).not.toHaveBeenCalled();
  });

  it("renders split Markdown preview from the unsaved buffer and opens document links", async () => {
    const onOpenSourceLink = vi.fn();

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "# Original\n\n[Guide](./guide.md#L10)\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onOpenSourceLink={onOpenSourceLink}
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    fireEvent.change(
      await screen.findByLabelText("Source editor for /repo/docs/readme.md"),
      {
        target: { value: "# Changed Title\n\n[Guide](./guide.md#L10)\n" },
      },
    );

    expect(await screen.findByRole("heading", { name: "Changed Title" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("link", { name: "Guide" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/docs/guide.md",
      line: 10,
      openInNewTab: false,
    });
  });

  it("round-trips Markdown modes and resets to code for non-Markdown files", async () => {
    const { rerender } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "# Mode Test\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    const codeEditor = await screen.findByLabelText("Source editor for /repo/docs/readme.md");
    expect(codeEditor).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    expect(await screen.findByRole("heading", { name: "Mode Test" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Source editor for /repo/docs/readme.md")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    expect(await screen.findByLabelText("Source editor for /repo/docs/readme.md")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Mode Test" })).toBeInTheDocument();

    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/src/main.rs",
          content: "fn main() {}\n",
          language: "rust",
        }}
        sourcePath="/repo/src/main.rs"
        onSaveFile={vi.fn()}
      />,
    );

    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Preview" })).not.toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Split" })).not.toBeInTheDocument();
      expect(screen.getByLabelText("Source editor for /repo/src/main.rs")).toBeInTheDocument();
    });

    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/docs/readme.md",
          content: "# Mode Test\n",
          language: "markdown",
        }}
        sourcePath="/repo/docs/readme.md"
        workspaceRoot="/repo"
        onSaveFile={vi.fn()}
      />,
    );

    expect(await screen.findByLabelText("Source editor for /repo/docs/readme.md")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Mode Test" })).not.toBeInTheDocument();
  });

  // Phase 3 of `docs/features/source-renderers.md`: non-Markdown
  // files with renderable regions expose Preview/Split too.
  // Dedicated Mermaid files (`.mmd`, `.mermaid`) surface a whole-file
  // region, so the mode toolbar appears and the chip reports the
  // renderer kind instead of just "Markdown".
  it("exposes Preview/Split for dedicated `.mmd` files and labels the chip as Mermaid", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/diagrams/flow.mmd",
          content: "flowchart TD\n  A --> B\n",
          language: null,
        }}
        sourcePath="/repo/diagrams/flow.mmd"
        onSaveFile={vi.fn()}
      />,
    );

    // Code button is present by default.
    expect(await screen.findByRole("button", { name: "Code" })).toBeInTheDocument();
    // Preview + Split surfaces because the registry detects a
    // whole-file Mermaid region.
    expect(screen.getByRole("button", { name: "Preview" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Split" })).toBeInTheDocument();
    // Chip says "Mermaid", not "Markdown".
    const chip = screen.getByText("Mermaid");
    expect(chip).toHaveClass("chip");

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    expect(await screen.findByLabelText("Rendered preview")).toBeInTheDocument();
    expect(await screen.findByText(/Lines 1.*3/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    expect(await screen.findByLabelText("Source editor for /repo/diagrams/flow.mmd")).toBeInTheDocument();
    expect(screen.getByText(/Lines 1.*3/)).toBeInTheDocument();
  });

  // A plain Rust file with no doc comments and no recognized content
  // has zero renderable regions (Phase 5 will change this for files
  // with doc-comment fenced blocks). Confirm Phase 3 does NOT
  // over-expose Preview/Split for such files.
  it("does not expose Preview/Split for plain Rust files with no renderable regions", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/src/plain.rs",
          content: "fn add(a: i32, b: i32) -> i32 { a + b }\n",
          language: "rust",
        }}
        sourcePath="/repo/src/plain.rs"
        onSaveFile={vi.fn()}
      />,
    );

    await screen.findByLabelText("Source editor for /repo/src/plain.rs");
    expect(screen.queryByRole("button", { name: "Preview" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Split" })).not.toBeInTheDocument();
  });

  // Phase 5 of `docs/features/source-renderers.md`: Rust files with
  // doc-comment fenced Mermaid/math blocks surface the Preview/Split
  // toolbar just like `.mmd` files. Plain Rust files (no doc
  // comments, or doc comments without fenced diagrams) still do NOT
  // expose the toolbar.
  it("exposes Preview/Split for Rust files with a Mermaid fence in a doc comment", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/src/architecture.rs",
          content: [
            "/// Architecture:",
            "///",
            "/// ```mermaid",
            "/// flowchart TD",
            "///   A --> B",
            "/// ```",
            "pub fn example() {}",
          ].join("\n"),
          language: "rust",
        }}
        sourcePath="/repo/src/architecture.rs"
        onSaveFile={vi.fn()}
      />,
    );

    // Preview + Split surface because the registry detected a
    // Mermaid region inside the `///` doc-comment block.
    expect(await screen.findByRole("button", { name: "Preview" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Split" })).toBeInTheDocument();
    // The chip labels the renderer kind.
    expect(screen.getByText("Mermaid")).toHaveClass("chip");

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    expect(await screen.findByLabelText("Rendered preview")).toBeInTheDocument();
    expect(await screen.findByText(/Lines 3.*6/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Split" }));
    expect(await screen.findByLabelText("Source editor for /repo/src/architecture.rs")).toBeInTheDocument();
    expect(screen.getByText(/Lines 3.*6/)).toBeInTheDocument();
  });

  // Inline rendering is now directly part of Code mode: the Source
  // panel hands detected renderable regions to `MonacoCodeEditor`
  // as `inlineZones`, which renders each diagram via a view zone
  // pinned after the region's last source line. No separate mode
  // toggle — the diagrams appear alongside the editable source
  // whenever regions exist.
  it("passes inline zones to the Code-mode Monaco editor when renderable regions are detected", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/src/architecture.rs",
          content: [
            "/// Architecture:",
            "///",
            "/// ```mermaid",
            "/// flowchart TD",
            "///   A --> B",
            "/// ```",
            "pub fn example() {}",
          ].join("\n"),
          language: "rust",
        }}
        sourcePath="/repo/src/architecture.rs"
        onSaveFile={vi.fn()}
      />,
    );

    const editor = await screen.findByLabelText(
      "Source editor for /repo/src/architecture.rs",
    );
    // Exactly one inline zone — the Mermaid fence in the `///` doc
    // comment. Pinned after line 6 (the closing ``` marker).
    expect(editor).toHaveAttribute("data-inline-zone-count", "1");
    expect(editor).toHaveAttribute("data-inline-zone-first-after-line", "6");
  });

  it("passes zero inline zones for files with no renderable regions", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/src/plain.rs",
          content: "fn add(a: i32, b: i32) -> i32 { a + b }\n",
          language: "rust",
        }}
        sourcePath="/repo/src/plain.rs"
        onSaveFile={vi.fn()}
      />,
    );

    const editor = await screen.findByLabelText(
      "Source editor for /repo/src/plain.rs",
    );
    // Non-renderable file: zero zones, zero first-line value.
    expect(editor).toHaveAttribute("data-inline-zone-count", "0");
  });

  it("recomputes inline zones from the CURRENT edit buffer, not the saved file content", async () => {
    const { rerender } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/diagrams/flow.mmd",
          content: "flowchart TD\n  A --> B\n",
          language: null,
        }}
        sourcePath="/repo/diagrams/flow.mmd"
        onSaveFile={vi.fn()}
      />,
    );

    const editor = await screen.findByLabelText(
      "Source editor for /repo/diagrams/flow.mmd",
    );
    expect(editor).toHaveAttribute("data-inline-zone-count", "1");
    // Whole-file `.mmd` region ends at line 3 (trailing newline
    // creates a third line in `content.split(/\r?\n/)`), so the
    // zone is pinned after line 3.
    expect(editor).toHaveAttribute("data-inline-zone-first-after-line", "3");

    // User types an additional flowchart node — the fence body
    // grows, and the zone's afterLineNumber shifts.
    fireEvent.change(editor, {
      target: { value: "flowchart TD\n  A --> B\n  B --> C\n" },
    });

    await waitFor(() => {
      expect(editor).toHaveAttribute(
        "data-inline-zone-first-after-line",
        "4",
      );
    });
  });

  // Edits that ADD renderable content (e.g., user types a Mermaid
  // fence into a previously-empty `.md` file) should expose
  // Preview/Split once the registry picks up the new region. The
  // existing Markdown file test already covers the static path; this
  // test exercises the re-detection on editor changes for a
  // non-Markdown file context where the preview would otherwise not
  // appear at all.
  it("re-detects renderable regions from the current editor buffer (not the saved content)", async () => {
    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/notes.mmd",
          content: "",
          language: null,
        }}
        sourcePath="/repo/notes.mmd"
        onSaveFile={vi.fn()}
      />,
    );

    const editor = await screen.findByLabelText(
      "Source editor for /repo/notes.mmd",
    );
    // Empty `.mmd` file has no renderable region, so no Preview.
    expect(screen.queryByRole("button", { name: "Preview" })).not.toBeInTheDocument();

    fireEvent.change(editor, {
      target: { value: "flowchart TD\n  A --> B\n" },
    });

    expect(await screen.findByRole("button", { name: "Preview" })).toBeInTheDocument();
  });

  // Inline-zone id stability contract (what this block pins):
  //
  // - For a Markdown file with a fenced Mermaid block, the id is
  //   `mermaid:${sameBodyOrdinal}:${quickHash(fence.body)}`
  //   (see `ui/src/source-renderers.ts::detectMarkdownRegions`).
  //   So the id is stable when edits outside the fence shift the
  //   absolute line numbers but leave the Mermaid body unchanged.
  //   The portal key in `MonacoCodeEditor` uses the id, so stability
  //   here is what keeps the Mermaid iframe DOM alive across
  //   keystrokes outside the diagram.
  //
  // - For a dedicated `.mmd` file, the id is
  //   `mermaid-file:${quickHash(context.content)}` — the WHOLE
  //   file content is hashed, so any edit flips the id and the
  //   diagram DOM is recreated. This is an intentional trade-off
  //   documented in the code; we pin it here so a future
  //   refactor doesn't silently drop the exception.
  //
  describe("inline-zone id stability", () => {
    it("keeps the zone id stable when the fence's line span and body are unchanged", async () => {
      render(
        <SourcePanel
          editorAppearance={editorAppearance}
          editorFontSizePx={14}
          fileState={{
            ...readyFileState,
            path: "/repo/docs/diagram.md",
            content: [
              "# Title",
              "",
              "```mermaid",
              "flowchart TD",
              "  A --> B",
              "```",
              "",
              "Footer.",
            ].join("\n"),
            language: "markdown",
          }}
          sourcePath="/repo/docs/diagram.md"
          onSaveFile={vi.fn()}
        />,
      );

      const editor = await screen.findByLabelText(
        "Source editor for /repo/docs/diagram.md",
      );
      expect(editor).toHaveAttribute("data-inline-zone-count", "1");
      const idsBeforeEdit = editor.getAttribute("data-inline-zone-ids");
      expect(idsBeforeEdit).toBeTruthy();

      // In-place footer rewrite: no fence-body change and no
      // same-body ordinal change, so the id stays stable.
      fireEvent.change(editor, {
        target: {
          value: [
            "# Title",
            "",
            "```mermaid",
            "flowchart TD",
            "  A --> B",
            "```",
            "",
            "Footer edited.",
          ].join("\n"),
        },
      });

      await waitFor(() => {
        // Wait for the edit to propagate: the editor value prop
        // should reflect the new content. Use this as a
        // synchronization point rather than asserting the id
        // directly (which might report stale during the update).
        expect((editor as HTMLTextAreaElement).value).toContain("Footer edited.");
      });
      expect(editor).toHaveAttribute("data-inline-zone-ids", idsBeforeEdit!);
    });

    it("changes the zone id when the fence body changes", async () => {
      render(
        <SourcePanel
          editorAppearance={editorAppearance}
          editorFontSizePx={14}
          fileState={{
            ...readyFileState,
            path: "/repo/docs/diagram.md",
            content: [
              "# Title",
              "",
              "```mermaid",
              "flowchart TD",
              "  A --> B",
              "```",
              "",
            ].join("\n"),
            language: "markdown",
          }}
          sourcePath="/repo/docs/diagram.md"
          onSaveFile={vi.fn()}
        />,
      );

      const editor = await screen.findByLabelText(
        "Source editor for /repo/docs/diagram.md",
      );
      const idsBeforeEdit = editor.getAttribute("data-inline-zone-ids");
      expect(idsBeforeEdit).toBeTruthy();

      // Edit the fence body — same start/end line, different
      // hash → id changes. This signals the portal to remount
      // so the Mermaid renderer picks up the new source.
      fireEvent.change(editor, {
        target: {
          value: [
            "# Title",
            "",
            "```mermaid",
            "flowchart TD",
            "  A --> C",
            "```",
            "",
          ].join("\n"),
        },
      });

      await waitFor(() => {
        expect((editor as HTMLTextAreaElement).value).toContain("A --> C");
      });
      const idsAfterEdit = editor.getAttribute("data-inline-zone-ids");
      expect(idsAfterEdit).toBeTruthy();
      expect(idsAfterEdit).not.toBe(idsBeforeEdit);
    });

    it("keeps the zone id stable when lines are inserted above the fence", async () => {
      render(
        <SourcePanel
          editorAppearance={editorAppearance}
          editorFontSizePx={14}
          fileState={{
            ...readyFileState,
            path: "/repo/docs/diagram.md",
            content: [
              "# Title",
              "",
              "```mermaid",
              "flowchart TD",
              "  A --> B",
              "```",
              "",
            ].join("\n"),
            language: "markdown",
          }}
          sourcePath="/repo/docs/diagram.md"
          onSaveFile={vi.fn()}
        />,
      );

      const editor = await screen.findByLabelText(
        "Source editor for /repo/docs/diagram.md",
      );
      const idsBeforeEdit = editor.getAttribute("data-inline-zone-ids");
      expect(idsBeforeEdit).toBeTruthy();

      // Insert a new paragraph BEFORE the fence. The absolute
      // source lines shift, but the fence body and same-body
      // ordinal are unchanged, so the inline zone remains stable.
      fireEvent.change(editor, {
        target: {
          value: [
            "# Title",
            "",
            "Preamble paragraph.",
            "",
            "```mermaid",
            "flowchart TD",
            "  A --> B",
            "```",
            "",
          ].join("\n"),
        },
      });

      await waitFor(() => {
        expect((editor as HTMLTextAreaElement).value).toContain("Preamble paragraph.");
      });
      const idsAfterEdit = editor.getAttribute("data-inline-zone-ids");
      expect(idsAfterEdit).toBeTruthy();
      expect(idsAfterEdit).toBe(idsBeforeEdit);
    });

    it("changes the zone id on any `.mmd` whole-file edit (documented exception)", async () => {
      // Whole-file `.mmd` regions hash the ENTIRE file content
      // (see `source-renderers.ts::detectWholeFileMermaidRegion`
      // — id is `mermaid-file:${quickHash(context.content)}`),
      // so ANY keystroke inside the file flips the id and the
      // diagram portal is remounted. This is an intentional
      // trade-off for `.mmd` files (the whole file IS the
      // diagram, so there's no stable "outside-the-fence"
      // region to key on). Pinned here so a refactor that
      // changes the id format has to consciously update this
      // test.
      render(
        <SourcePanel
          editorAppearance={editorAppearance}
          editorFontSizePx={14}
          fileState={{
            ...readyFileState,
            path: "/repo/diagrams/flow.mmd",
            content: "flowchart TD\n  A --> B\n",
            language: null,
          }}
          sourcePath="/repo/diagrams/flow.mmd"
          onSaveFile={vi.fn()}
        />,
      );

      const editor = await screen.findByLabelText(
        "Source editor for /repo/diagrams/flow.mmd",
      );
      const idsBeforeEdit = editor.getAttribute("data-inline-zone-ids");
      expect(idsBeforeEdit).toMatch(/^mermaid-file:/);

      fireEvent.change(editor, {
        target: { value: "flowchart TD\n  A --> B\n  B --> C\n" },
      });

      await waitFor(() => {
        expect((editor as HTMLTextAreaElement).value).toContain("B --> C");
      });
      const idsAfterEdit = editor.getAttribute("data-inline-zone-ids");
      expect(idsAfterEdit).toMatch(/^mermaid-file:/);
      expect(idsAfterEdit).not.toBe(idsBeforeEdit);
    });
  });

  it("auto-rebases the latest editor buffer after a disk refresh returns", async () => {
    const latestFile = createDeferred<SourceFileState>();
    const onFetchLatestFile = vi.fn(() => latestFile.promise);
    const onAdoptFileState = vi.fn();
    const { rerender } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          content: "alpha\nbeta\n",
          contentHash: "sha256:base",
        }}
        sourcePath="src/main.rs"
        onFetchLatestFile={onFetchLatestFile}
        onAdoptFileState={onAdoptFileState}
        onSaveFile={vi.fn()}
      />,
    );

    const editor = await screen.findByLabelText("Source editor for src/main.rs");
    fireEvent.change(editor, {
      target: { value: "alpha local\nbeta\n" },
    });
    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          content: "alpha\nbeta\n",
          contentHash: "sha256:base",
          staleOnDisk: true,
          externalContentHash: "sha256:disk",
        }}
        sourcePath="src/main.rs"
        onFetchLatestFile={onFetchLatestFile}
        onAdoptFileState={onAdoptFileState}
        onSaveFile={vi.fn()}
      />,
    );

    await waitFor(() => {
      expect(onFetchLatestFile).toHaveBeenCalledWith("src/main.rs");
    });
    fireEvent.change(editor, {
      target: { value: "alpha latest\nbeta\n" },
    });
    latestFile.resolve({
      ...readyFileState,
      content: "alpha\nbeta disk\n",
      contentHash: "sha256:disk",
    });

    await waitFor(() => {
      expect(editor).toHaveValue("alpha latest\nbeta disk\n");
    });
    expect(onAdoptFileState).toHaveBeenCalledWith(
      expect.objectContaining({
        content: "alpha\nbeta disk\n",
        contentHash: "sha256:disk",
      }),
    );
  });

  it("can intentionally overwrite after a disk-change conflict", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);
    const nextContent = "fn main() { println!(\"mine\"); }\n";

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          staleOnDisk: true,
          externalContentHash: "sha256:disk",
        }}
        sourcePath="src/main.rs"
        onSaveFile={onSaveFile}
      />,
    );

    fireEvent.change(
      await screen.findByLabelText("Source editor for src/main.rs"),
      {
        target: { value: nextContent },
      },
    );
    fireEvent.click(
      await screen.findByRole("button", { name: "Save anyway" }),
    );

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith("src/main.rs", nextContent, {
        overwrite: true,
      });
    });
  });

  it("preserves a deleted disk file buffer and can restore it", async () => {
    const onSaveFile = vi.fn().mockResolvedValue(undefined);

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          staleOnDisk: true,
          externalChangeKind: "deleted",
          externalContentHash: null,
        }}
        sourcePath="src/main.rs"
        onSaveFile={onSaveFile}
      />,
    );

    expect(screen.getByText("File deleted on disk")).toBeInTheDocument();
    expect(
      await screen.findByLabelText("Source editor for src/main.rs"),
    ).toHaveValue(readyFileState.content);

    fireEvent.click(screen.getByRole("button", { name: "Restore file" }));

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "src/main.rs",
        readyFileState.content,
        { overwrite: true },
      );
    });
  });

  it("opens a disk-to-buffer compare view for dirty stale files", async () => {
    const onFetchLatestFile = vi.fn().mockResolvedValue({
      ...readyFileState,
      content: "fn main() { println!(\"disk\"); }\n",
      contentHash: "sha256:disk",
    });

    render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          staleOnDisk: true,
          externalContentHash: "sha256:disk",
        }}
        sourcePath="src/main.rs"
        onFetchLatestFile={onFetchLatestFile}
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.change(
      await screen.findByLabelText("Source editor for src/main.rs"),
      {
        target: { value: "fn main() { println!(\"mine\"); }\n" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "Compare" }));

    expect(
      await screen.findByLabelText("Source compare for src/main.rs"),
    ).toBeInTheDocument();
    expect(screen.getByTestId("diff-original")).toHaveTextContent("disk");
    expect(screen.getByTestId("diff-modified")).toHaveTextContent("mine");
  });

  it("ignores stale compare results after the source tab changes", async () => {
    const latestFile = createDeferred<SourceFileState>();
    const onFetchLatestFile = vi.fn().mockReturnValue(latestFile.promise);

    const { rerender } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "src/first.rs",
          staleOnDisk: true,
          externalContentHash: "sha256:first-disk",
        }}
        sourcePath="src/first.rs"
        onFetchLatestFile={onFetchLatestFile}
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.change(
      await screen.findByLabelText("Source editor for src/first.rs"),
      {
        target: { value: "fn first() { println!(\"mine\"); }\n" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "Compare" }));

    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "src/second.rs",
          content: "fn second() {}\n",
          contentHash: "sha256:second",
        }}
        sourcePath="src/second.rs"
        onFetchLatestFile={onFetchLatestFile}
        onSaveFile={vi.fn()}
      />,
    );

    latestFile.resolve({
      ...readyFileState,
      path: "src/first.rs",
      content: "fn first() { println!(\"disk\"); }\n",
      contentHash: "sha256:first-disk",
    });

    await waitFor(() => {
      expect(screen.getByLabelText("Source editor for src/second.rs")).toBeInTheDocument();
    });
    expect(screen.queryByLabelText("Source compare for src/second.rs")).not.toBeInTheDocument();
    expect(screen.queryByTestId("diff-original")).not.toBeInTheDocument();
  });

  it("ignores stale copy-path completions after the source tab changes", async () => {
    const copiedPath = createDeferred<void>();
    copyTextToClipboardMock.mockReturnValue(copiedPath.promise);

    const { rerender } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "src/first.rs",
          content: "fn first() {}\n",
          contentHash: "sha256:first",
        }}
        sourcePath="src/first.rs"
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Copy path" }));
    await waitFor(() => {
      expect(copyTextToClipboardMock).toHaveBeenCalledWith("src/first.rs");
    });

    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "src/second.rs",
          content: "fn second() {}\n",
          contentHash: "sha256:second",
        }}
        sourcePath="src/second.rs"
        onSaveFile={vi.fn()}
      />,
    );

    await act(async () => {
      copiedPath.resolve();
      await copiedPath.promise;
    });

    expect(screen.getByText("src/second.rs")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy path" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Path copied" })).not.toBeInTheDocument();
  });

  it("adopts save responses only while the saved source path is still active", async () => {
    const savedFile = createDeferred<SourceFileState>();
    const onSaveFile = vi.fn().mockReturnValue(savedFile.promise);
    const onAdoptFileState = vi.fn();

    const { rerender } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "src/first.rs",
          content: "fn first() {}\n",
          contentHash: "sha256:first",
        }}
        sourcePath="src/first.rs"
        onAdoptFileState={onAdoptFileState}
        onSaveFile={onSaveFile}
      />,
    );

    const editor = await screen.findByLabelText("Source editor for src/first.rs");
    fireEvent.change(editor, {
      target: { value: "fn first() { println!(\"mine\"); }\n" },
    });
    fireEvent.keyDown(editor, { ctrlKey: true, key: "s" });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "src/first.rs",
        "fn first() { println!(\"mine\"); }\n",
        undefined,
      );
    });

    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "src/second.rs",
          content: "fn second() {}\n",
          contentHash: "sha256:second",
        }}
        sourcePath="src/second.rs"
        onAdoptFileState={onAdoptFileState}
        onSaveFile={onSaveFile}
      />,
    );

    await act(async () => {
      savedFile.resolve({
        ...readyFileState,
        path: "src/first.rs",
        content: "fn first() { println!(\"mine\"); }\n",
        contentHash: "sha256:first-saved",
      });
      await savedFile.promise;
    });

    expect(onAdoptFileState).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Source editor for src/second.rs")).toHaveValue(
      "fn second() {}\n",
    );
  });

  it("preserves edits typed while adopting a same-path save response", async () => {
    const savedFile = createDeferred<SourceFileState>();
    const onSaveFile = vi.fn().mockReturnValue(savedFile.promise);
    const onAdoptFileState = vi.fn();
    const saveStartedContent = "fn main() {\n  println!(\"before\");\n}\n";
    const liveContentAfterSaveStarted =
      "fn main() {\n  println!(\"before\");\n  println!(\"after\");\n}\n";
    const savedFileState: SourceFileState = {
      ...readyFileState,
      content: saveStartedContent,
      contentHash: "sha256:saved",
      sizeBytes: saveStartedContent.length,
    };

    function Harness() {
      const [currentFileState, setCurrentFileState] =
        useState<SourceFileState>(readyFileState);
      return (
        <SourcePanel
          editorAppearance={editorAppearance}
          editorFontSizePx={14}
          fileState={currentFileState}
          sourcePath="src/main.rs"
          onAdoptFileState={(nextFileState) => {
            onAdoptFileState(nextFileState);
            setCurrentFileState(nextFileState);
          }}
          onSaveFile={onSaveFile}
        />
      );
    }

    render(<Harness />);

    const editor = await screen.findByLabelText("Source editor for src/main.rs");
    fireEvent.change(editor, {
      target: { value: saveStartedContent },
    });
    fireEvent.keyDown(editor, { ctrlKey: true, key: "s" });

    await waitFor(() => {
      expect(onSaveFile).toHaveBeenCalledWith(
        "src/main.rs",
        saveStartedContent,
        undefined,
      );
    });

    fireEvent.change(editor, {
      target: { value: liveContentAfterSaveStarted },
    });

    await act(async () => {
      savedFile.resolve(savedFileState);
      await savedFile.promise;
    });

    await waitFor(() => {
      expect(onAdoptFileState).toHaveBeenCalledWith(savedFileState);
      expect(screen.getByLabelText("Source editor for src/main.rs")).toHaveValue(
        liveContentAfterSaveStarted,
      );
    });
  });

  it("adopts same-path reload responses", async () => {
    const reloadedFile = createDeferred<SourceFileState>();
    const onReloadFile = vi.fn().mockReturnValue(reloadedFile.promise);
    const onAdoptFileState = vi.fn();
    const reloadedFileState: SourceFileState = {
      ...readyFileState,
      content: "fn main() {\n  println!(\"disk\");\n}\n",
      contentHash: "sha256:disk",
      staleOnDisk: false,
      externalContentHash: null,
      sizeBytes: 37,
    };

    function Harness() {
      const [currentFileState, setCurrentFileState] = useState<SourceFileState>({
        ...readyFileState,
        staleOnDisk: true,
        externalContentHash: "sha256:disk",
      });
      return (
        <SourcePanel
          editorAppearance={editorAppearance}
          editorFontSizePx={14}
          fileState={currentFileState}
          sourcePath="src/main.rs"
          onAdoptFileState={(nextFileState) => {
            onAdoptFileState(nextFileState);
            setCurrentFileState(nextFileState);
          }}
          onReloadFile={onReloadFile}
          onSaveFile={vi.fn()}
        />
      );
    }

    render(<Harness />);

    fireEvent.click(await screen.findByRole("button", { name: "Reload from disk" }));
    await waitFor(() => {
      expect(onReloadFile).toHaveBeenCalledWith("src/main.rs");
    });

    await act(async () => {
      reloadedFile.resolve(reloadedFileState);
      await reloadedFile.promise;
    });

    await waitFor(() => {
      expect(onAdoptFileState).toHaveBeenCalledWith(reloadedFileState);
      expect(screen.getByLabelText("Source editor for src/main.rs")).toHaveValue(
        reloadedFileState.content,
      );
    });
    expect(screen.queryByText("File changed on disk")).not.toBeInTheDocument();
  });

  it("adopts reload responses only while the reloaded source path is still active", async () => {
    const reloadedFile = createDeferred<SourceFileState>();
    const onReloadFile = vi.fn().mockReturnValue(reloadedFile.promise);
    const onAdoptFileState = vi.fn();

    const { rerender } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "src/first.rs",
          staleOnDisk: true,
          externalContentHash: "sha256:first-disk",
        }}
        sourcePath="src/first.rs"
        onAdoptFileState={onAdoptFileState}
        onReloadFile={onReloadFile}
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Reload from disk" }));
    await waitFor(() => {
      expect(onReloadFile).toHaveBeenCalledWith("src/first.rs");
    });

    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "src/second.rs",
          content: "fn second() {}\n",
          contentHash: "sha256:second",
        }}
        sourcePath="src/second.rs"
        onAdoptFileState={onAdoptFileState}
        onReloadFile={onReloadFile}
        onSaveFile={vi.fn()}
      />,
    );

    await act(async () => {
      reloadedFile.resolve({
        ...readyFileState,
        path: "src/first.rs",
        content: "fn first() { println!(\"disk\"); }\n",
        contentHash: "sha256:first-disk",
      });
      await reloadedFile.promise;
    });

    expect(onAdoptFileState).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Source editor for src/second.rs")).toHaveValue(
      "fn second() {}\n",
    );
  });

  it("automatically rebases dirty buffers when disk changes do not overlap", async () => {
    const baseFileState: SourceFileState = {
      ...readyFileState,
      content: "alpha\nbeta\ngamma\n",
      contentHash: "sha256:base",
    };
    const diskFileState: SourceFileState = {
      ...baseFileState,
      content: "alpha\nbeta\nagent\ngamma\n",
      contentHash: "sha256:disk",
      staleOnDisk: false,
      externalContentHash: null,
    };
    const onFetchLatestFile = vi.fn().mockResolvedValue(diskFileState);
    const onAdoptFileState = vi.fn();

    const { rerender } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={baseFileState}
        sourcePath="src/main.rs"
        onAdoptFileState={onAdoptFileState}
        onFetchLatestFile={onFetchLatestFile}
        onSaveFile={vi.fn()}
      />,
    );

    const editor = await screen.findByLabelText("Source editor for src/main.rs");
    fireEvent.change(editor, {
      target: { value: "alpha\nuser\nbeta\ngamma\n" },
    });

    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...baseFileState,
          staleOnDisk: true,
          externalContentHash: "sha256:disk",
        }}
        sourcePath="src/main.rs"
        onAdoptFileState={onAdoptFileState}
        onFetchLatestFile={onFetchLatestFile}
        onSaveFile={vi.fn()}
      />,
    );

    await waitFor(() => {
      expect(onAdoptFileState).toHaveBeenCalledWith(diskFileState);
      expect(editor).toHaveValue("alpha\nuser\nbeta\nagent\ngamma\n");
    });
  });

  it("does not adopt auto-rebase state after unmount", async () => {
    const latestFile = createDeferred<SourceFileState>();
    const onFetchLatestFile = vi.fn(() => latestFile.promise);
    const onAdoptFileState = vi.fn();
    const baseFileState: SourceFileState = {
      ...readyFileState,
      content: "alpha\nbeta\n",
      contentHash: "sha256:base",
    };

    const { rerender, unmount } = render(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={baseFileState}
        sourcePath="src/main.rs"
        onAdoptFileState={onAdoptFileState}
        onFetchLatestFile={onFetchLatestFile}
        onSaveFile={vi.fn()}
      />,
    );

    fireEvent.change(await screen.findByLabelText("Source editor for src/main.rs"), {
      target: { value: "alpha local\nbeta\n" },
    });
    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...baseFileState,
          staleOnDisk: true,
          externalContentHash: "sha256:disk",
        }}
        sourcePath="src/main.rs"
        onAdoptFileState={onAdoptFileState}
        onFetchLatestFile={onFetchLatestFile}
        onSaveFile={vi.fn()}
      />,
    );

    await waitFor(() => {
      expect(onFetchLatestFile).toHaveBeenCalledWith("src/main.rs");
    });
    unmount();
    await act(async () => {
      latestFile.resolve({
        ...baseFileState,
        content: "alpha\nbeta disk\n",
        contentHash: "sha256:disk",
      });
      await Promise.resolve();
    });

    expect(onAdoptFileState).not.toHaveBeenCalled();
  });

  it("rebases local edits onto non-overlapping disk changes", () => {
    const result = rebaseContentOntoDisk(
      "alpha\nbeta\ngamma\n",
      "alpha\nuser\nbeta\ngamma\n",
      "alpha\nbeta\nagent\ngamma\n",
    );

    expect(result).toEqual({
      status: "clean",
      content: "alpha\nuser\nbeta\nagent\ngamma\n",
    });
  });

  it("rejects overlapping local and disk edits", () => {
    const result = rebaseContentOntoDisk(
      "alpha\nbeta\ngamma\n",
      "alpha\nuser\ngamma\n",
      "alpha\nagent\ngamma\n",
    );

    expect(result.status).toBe("conflict");
  });

  it("rebases empty-file edits when only one side changed", () => {
    expect(rebaseContentOntoDisk("", "local\n", "")).toEqual({
      status: "clean",
      content: "local\n",
    });
    expect(rebaseContentOntoDisk("", "", "disk\n")).toEqual({
      status: "clean",
      content: "disk\n",
    });
  });

  it("deduplicates identical local and disk edits as a no-op merge", () => {
    expect(
      rebaseContentOntoDisk(
        "alpha\nbeta\n",
        "alpha\nshared\n",
        "alpha\nshared\n",
      ),
    ).toEqual({
      status: "clean",
      content: "alpha\nshared\n",
    });
  });

  it("rejects merges that exceed the diff cell guard", () => {
    const largeBase = Array.from({ length: 2000 }, (_, index) => `base ${index}\n`).join("");
    const largeLocal = Array.from({ length: 2000 }, (_, index) => `local ${index}\n`).join("");

    const result = rebaseContentOntoDisk(largeBase, largeLocal, largeBase);

    expect(result).toEqual({
      status: "conflict",
      reason:
        "Could not apply edits automatically because the file is too large to merge safely.",
    });
  });
});
