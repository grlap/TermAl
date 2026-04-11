import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { copyTextToClipboard } from "../clipboard";
import {
  SourcePanel,
  rebaseContentOntoDisk,
  type SourceFileState,
} from "./SourcePanel";

vi.mock("../clipboard", () => ({
  copyTextToClipboard: vi.fn(),
}));

vi.mock("../MonacoCodeEditor", () => ({
  MonacoCodeEditor: ({
    ariaLabel,
    onChange,
    value,
  }: {
    ariaLabel: string;
    onChange: (nextValue: string) => void;
    value: string;
  }) => (
    <textarea
      aria-label={ariaLabel}
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
});
