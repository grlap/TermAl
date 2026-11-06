// Watcher delivery through the source hook into the actual open panel.
// The API models a loaded file which disappears before the next notification.
import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fetchFile } from "./api";
import { useSessionPaneSourceFileState } from "./SessionPaneView.source-file";
import { SourcePanel } from "./panels/SourcePanel";
import type { WorkspaceFilesChangedEvent } from "./types";

vi.mock("./api", async () => {
  const actual = await vi.importActual<typeof import("./api")>("./api");
  return { ...actual, fetchFile: vi.fn() };
});
vi.mock("./MonacoCodeEditor", () => ({
  MonacoCodeEditor: ({ ariaLabel, value }: { ariaLabel: string; value: string }) =>
    <textarea aria-label={ariaLabel} value={value} readOnly />,
}));

const path = "/repo/transient.ts";
const fetchFileMock = vi.mocked(fetchFile);

function OpenSource({ event }: { event: WorkspaceFilesChangedEvent | null }) {
  const source = useSessionPaneSourceFileState({
    activeSourceOriginProjectId: null,
    activeSourceOriginSessionId: "session-1",
    activeSourceTab: null,
    activeSourceWorkspaceRoot: "/repo",
    onPaneSourcePathChange: () => {},
    paneId: "source",
    paneSourcePath: path,
    paneViewMode: "source",
    sourceCandidatePaths: [path],
    workspaceFilesChangedEvent: event,
  });
  return <>
    <output data-testid="source-load-error">{source.fileState.error ?? ""}</output>
    <SourcePanel editorAppearance="light" editorFontSizePx={14}
      fileState={source.fileState} sourcePath={path} onSaveFile={async () => {}} />
  </>;
}

describe("open source watcher disappearance", () => {
  beforeEach(() => { fetchFileMock.mockReset(); });

  it.each(["modified", "subtree"] as const)("keeps a missing-file state without a load error after %s", async (kind) => {
    fetchFileMock.mockResolvedValueOnce({
      path, content: "const remembered = true;\n", contentHash: "sha256:initial", language: "typescript",
    }).mockRejectedValue(new Error("File not found"));
    const { rerender } = render(<OpenSource event={null} />);
    expect(await screen.findByLabelText(`Source editor for ${path}`)).toHaveValue("const remembered = true;\n");
    // Modified is the exact create/delete coalescing result. Subtree also
    // covers Windows Remove(Any), where the backend cannot identify a leaf.
    rerender(<OpenSource event={{ revision: 1, changes: [{ path, kind }] }} />);
    expect(await screen.findByText("File deleted on disk")).toBeInTheDocument();
    expect(screen.getByLabelText(`Source editor for ${path}`)).toHaveValue("const remembered = true;\n");
    expect(screen.getByTestId("source-load-error")).toBeEmptyDOMElement();
    expect(screen.getByRole("button", { name: "Restore file" })).toBeInTheDocument();
    await waitFor(() => expect(fetchFileMock).toHaveBeenCalledTimes(2));
  });
});
