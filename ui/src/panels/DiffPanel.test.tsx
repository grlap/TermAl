import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { forwardRef, useEffect, useImperativeHandle, type ForwardedRef } from "react";
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
    ref: ForwardedRef<{ goToNextChange: () => void; goToPreviousChange: () => void }>,
  ) {
    useImperativeHandle(ref, () => ({
      goToNextChange: () => {},
      goToPreviousChange: () => {},
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
  MonacoCodeEditor: ({
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
  }) => {
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
  },
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
