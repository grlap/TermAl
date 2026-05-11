import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { buildDiffPreviewModel } from "../diff-preview";
import { MarkdownDiffView } from "./markdown-diff-view";
import type { MarkdownDiffPreviewModel } from "./markdown-diff-segments";

function markdownPreview(
  beforeContent: string,
  afterContent: string,
): MarkdownDiffPreviewModel {
  return {
    before: {
      completeness: "full",
      content: beforeContent,
      note: null,
      source: "head",
    },
    after: {
      completeness: "full",
      content: afterContent,
      note: null,
      source: "index",
    },
  };
}

function renderMarkdownDiffView(preview: MarkdownDiffPreviewModel) {
  const scrollRef = { current: null as HTMLDivElement | null };
  const props = {
    appearance: "dark" as const,
    canEdit: true,
    documentPath: "/repo/README.md",
    editBlockedReason: null,
    gitSectionId: "staged" as const,
    isDirty: false,
    isSaving: false,
    markdownPreview: preview,
    onCommitRenderedMarkdownDrafts: () => true,
    onCommitRenderedMarkdownSectionDraft: () => true,
    onOpenSourceLink: () => {},
    onRegisterRenderedMarkdownCommitter: () => () => {},
    onRenderedMarkdownSectionDraftChange: () => {},
    onSave: () => {},
    preview: buildDiffPreviewModel("", "edit"),
    saveStateLabel: null,
    scrollRef,
    workspaceRoot: "/repo",
  };

  const view = render(<MarkdownDiffView {...props} />);

  return {
    rerender: (nextPreview: MarkdownDiffPreviewModel) => {
      view.rerender(<MarkdownDiffView {...props} markdownPreview={nextPreview} />);
    },
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("MarkdownDiffView", () => {
  it("clamps the current change index when the segment set shrinks", async () => {
    const scrollIntoViewMock = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(() => {});

    const { rerender } = renderMarkdownDiffView(
      markdownPreview(
        "Intro\nOld first\nShared\nOld second\nOutro\n",
        "Intro\nNew first\nShared\nNew second\nOutro\n",
      ),
    );

    await waitFor(() => {
      expect(screen.getByText("Change 1 of 2")).toBeInTheDocument();
    });

    await act(async () => {
      screen.getByRole("button", { name: "Next change" }).click();
    });
    expect(screen.getByText("Change 2 of 2")).toBeInTheDocument();

    rerender(
      markdownPreview(
        "Intro\nOld first\nShared\nNew second\nOutro\n",
        "Intro\nNew first\nShared\nNew second\nOutro\n",
      ),
    );

    await waitFor(() => {
      expect(screen.getByText("Change 1 of 1")).toBeInTheDocument();
    });
    scrollIntoViewMock.mockClear();

    await act(async () => {
      screen.getByRole("button", { name: "Next change" }).click();
    });
    expect(screen.getByText("Change 1 of 1")).toBeInTheDocument();
    expect(scrollIntoViewMock).toHaveBeenCalledWith({ block: "center" });
  });
});
