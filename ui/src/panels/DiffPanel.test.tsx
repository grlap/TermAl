import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  MonacoDiffEditor: ({ modifiedValue, originalValue }: { modifiedValue: string; originalValue: string }) => (
    <div data-testid="monaco-diff-editor">{`${originalValue}=>${modifiedValue}`}</div>
  ),
}));

vi.mock("../MonacoCodeEditor", () => ({
  MonacoCodeEditor: ({ readOnly, value }: { readOnly?: boolean; value: string }) => (
    <div data-read-only={String(Boolean(readOnly))} data-testid="monaco-code-editor">
      {value}
    </div>
  ),
}));

const fetchFileMock = vi.mocked(fetchFile);

describe("DiffPanel", () => {
  beforeEach(() => {
    fetchFileMock.mockReset();
  });

  it("shows change stats and switches to the latest file view", async () => {
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
          summary="Updated example file"
        />,
      );
    });

    expect(screen.getByText("Changed 1")).toBeInTheDocument();
    expect(screen.getByText("Added 1")).toBeInTheDocument();
    expect(await screen.findByTestId("monaco-diff-editor")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Latest file" }));

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledWith("/repo/src/example.ts");
    });

    expect(await screen.findByTestId("monaco-code-editor")).toHaveTextContent("const latest = true;");
    expect(screen.getByTestId("monaco-code-editor")).toHaveAttribute("data-read-only", "true");
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
          summary="Updated example file"
        />,
      ));
    });

    await waitFor(() => {
      expect(fetchFileMock).toHaveBeenCalledWith("/repo/src/example.ts");
    });
    expect(await screen.findByTestId("monaco-diff-editor")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Raw patch" }));

    expect(container.querySelector(".diff-preview-raw-line-added")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-removed")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-hunk")).not.toBeNull();
    expect(container.querySelector(".diff-preview-raw-line-meta")).not.toBeNull();
  });
});
