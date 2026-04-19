import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { copyTextToClipboard } from "../clipboard";
import {
  SourcePanel,
  type SourceFileState,
} from "./SourcePanel";
import { rebaseContentOntoDisk } from "./content-rebase";

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
    value,
  }: {
    ariaLabel: string;
    inlineZones?: Array<{ id: string; afterLineNumber: number }>;
    onChange: (nextValue: string) => void;
    value: string;
  }) => (
    <textarea
      aria-label={ariaLabel}
      data-inline-zone-count={inlineZones?.length ?? 0}
      data-inline-zone-first-after-line={inlineZones?.[0]?.afterLineNumber ?? ""}
      data-inline-zone-ids={inlineZones?.map((zone) => zone.id).join(",") ?? ""}
      onChange={(event) => onChange(event.currentTarget.value)}
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

    fireEvent.change(
      await screen.findByLabelText("Source editor for src/main.rs"),
      {
        target: { value: "fn main() { println!(\"changed\"); }\n" },
      },
    );

    await waitFor(() => {
      expect(onDirtyChange).toHaveBeenLastCalledWith(true);
    });
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
    const { rerender } = render(
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

    // Empty `.mmd` file has no renderable region, so no Preview.
    expect(screen.queryByRole("button", { name: "Preview" })).not.toBeInTheDocument();

    // Saved-content update that introduces a Mermaid diagram should
    // expose the mode switcher.
    rerender(
      <SourcePanel
        editorAppearance={editorAppearance}
        editorFontSizePx={14}
        fileState={{
          ...readyFileState,
          path: "/repo/notes.mmd",
          content: "flowchart TD\n  A --> B\n",
          language: null,
        }}
        sourcePath="/repo/notes.mmd"
        onSaveFile={vi.fn()}
      />,
    );

    expect(await screen.findByRole("button", { name: "Preview" })).toBeInTheDocument();
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
