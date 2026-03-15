import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { forwardRef, useEffect, useImperativeHandle, type ForwardedRef } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchFile } from "../api";
import { DiffPanel } from "./DiffPanel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    fetchFile: vi.fn(),
  };
});

vi.mock("../MonacoDiffEditor", () => ({
  MonacoDiffEditor: forwardRef(function MonacoDiffEditorMock(
    {
      modifiedValue,
      originalValue,
    }: {
      modifiedValue: string;
      originalValue: string;
    },
    ref: ForwardedRef<{ goToNextChange: () => void; goToPreviousChange: () => void }>,
  ) {
    useImperativeHandle(ref, () => ({
      goToNextChange: () => {},
      goToPreviousChange: () => {},
    }));

    return <div data-testid="monaco-diff-editor">{`${originalValue}=>${modifiedValue}`}</div>;
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

describe("DiffPanel", () => {
  beforeEach(() => {
    fetchFileMock.mockReset();
  });

  it("shows compact diff stats and switches to edit mode", async () => {
    fetchFileMock.mockResolvedValue({
      content: "const latest = true;\n",
      language: "typescript",
      path: "/repo/src/example.ts",
    });

    await act(async () => {
      render(
        <DiffPanel
          appearance="dark"
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
          language="typescript"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      );
    });

    expect(screen.getByText("1")).toBeInTheDocument();
    expect(screen.getByText("+1")).toBeInTheDocument();
    expect(await screen.findByTestId("monaco-diff-editor")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Edit mode" }));

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledWith("/repo/src/example.ts");
    });

    expect(await screen.findByTestId("monaco-code-editor")).toHaveValue("const latest = true;\n");
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
          changeType="edit"
          diff={["@@ -1 +1 @@", "-old line", "+new line"].join("\n")}
          diffMessageId="diff-edit"
          filePath="/repo/src/example.ts"
          language="typescript"
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      );
    });

    fireEvent.click(screen.getByRole("button", { name: "Edit mode" }));
    const editor = await screen.findByTestId("monaco-code-editor");
    fireEvent.change(editor, { target: { value: "changed\n" } });

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
          onOpenPath={() => {}}
          onSaveFile={async () => {}}
          summary="Updated example file"
        />,
      ));
    });

    fireEvent.click(screen.getByRole("button", { name: "Raw patch" }));

    expect(container.querySelector(".diff-preview-raw-line-added")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-removed")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-hunk")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-meta")).not.toBeNull();
  });
});