import { afterEach, describe, expect, it, vi } from "vitest";

import {
  hasRoomForWorkspaceViewerSplit,
  workspacePaneHasRoomForViewerSplit,
} from "./workspace-viewer-layout";

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.replaceChildren();
  document.documentElement.style.removeProperty(
    "--workspace-viewer-pane-min-width",
  );
});

describe("workspace viewer layout", () => {
  it("requires enough width for both panes and their divider", () => {
    expect(hasRoomForWorkspaceViewerSplit(0, 480)).toBe(false);
    expect(hasRoomForWorkspaceViewerSplit(Number.NaN, 480)).toBe(false);
    expect(hasRoomForWorkspaceViewerSplit(975, 480)).toBe(false);
    expect(hasRoomForWorkspaceViewerSplit(976, 480)).toBe(true);
  });

  it("reads the rendered opener-pane width", () => {
    document.documentElement.style.setProperty(
      "--workspace-viewer-pane-min-width",
      "480px",
    );
    const pane = document.createElement("section");
    pane.className = "workspace-pane";
    pane.dataset.workspacePaneId = "pane-a";
    document.body.appendChild(pane);
    Object.defineProperty(pane, "getBoundingClientRect", {
      value: () => ({
        width: 800,
        height: 600,
        top: 0,
        right: 800,
        bottom: 600,
        left: 0,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      }),
    });

    expect(workspacePaneHasRoomForViewerSplit("pane-a")).toBe(false);
  });

  it("does not let compact density shrink the editor readability floor", () => {
    document.documentElement.style.setProperty(
      "--workspace-viewer-pane-min-width",
      "360px",
    );
    const pane = document.createElement("section");
    pane.className = "workspace-pane";
    pane.dataset.workspacePaneId = "pane-compact";
    document.body.appendChild(pane);
    Object.defineProperty(pane, "getBoundingClientRect", {
      value: () => ({
        width: 800,
        height: 600,
        top: 0,
        right: 800,
        bottom: 600,
        left: 0,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      }),
    });

    expect(workspacePaneHasRoomForViewerSplit("pane-compact")).toBe(false);
  });

  it("refuses the split when layout geometry is not measurable", () => {
    expect(workspacePaneHasRoomForViewerSplit("missing-pane")).toBe(false);
  });

  it("refuses the split when DOM geometry is unavailable", () => {
    vi.stubGlobal("document", undefined);

    expect(workspacePaneHasRoomForViewerSplit("pane-a")).toBe(false);
  });
});
