import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";

import { createRef } from "react";

import { createControlPanelSectionLauncherTab } from "../control-surface-state";
import { ControlPanelSurface, type ControlPanelSurfaceHandle } from "./ControlPanelSurface";

describe("ControlPanelSurface", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it("switches sections from the activity rail", () => {
    renderSurface();

    expect(screen.getByRole("heading", { level: 2, name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("sessions");

    fireEvent.click(screen.getByRole("button", { name: "Files" }));

    expect(screen.getByRole("heading", { level: 2, name: "Files" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("files");

    fireEvent.click(screen.getByRole("button", { name: "Projects" }));

    expect(screen.getByRole("heading", { level: 2, name: "Projects" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("projects");

    fireEvent.click(screen.getByRole("button", { name: "Orchestrators" }));

    expect(screen.getByRole("heading", { level: 2, name: "Orchestrators" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("orchestrators");

    fireEvent.click(screen.getByRole("button", { name: "Git status" }));

    expect(screen.getByRole("heading", { level: 2, name: "Git status" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("git");

    fireEvent.click(screen.getByRole("button", { name: "Board" }));

    expect(screen.getByRole("heading", { level: 2, name: "Board" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("board");
  });

  it("appends Board for users whose stored v2 order predates it", () => {
    // Migration proof: a real pre-board v2 stored order must surface the new
    // section automatically (normalizer appends missing defaults) and keep
    // the user's chosen order for the rest (review, mailbox #238-3).
    window.localStorage.setItem(
      "termal-control-panel-section-order-v2",
      JSON.stringify(["git", "files", "projects", "sessions", "orchestrators"]),
    );

    renderSurface();

    expect(getDockSectionLabels()).toEqual([
      "Git status",
      "Files",
      "Projects",
      "Sessions",
      "Orchestrators",
      "Board",
    ]);
  });

  it("opens preferences from the dock without switching sections", () => {
    const onOpenPreferences = vi.fn();

    renderSurface({ onOpenPreferences });

    fireEvent.click(screen.getByRole("button", { name: "Open preferences" }));

    expect(onOpenPreferences).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("heading", { level: 2, name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("sessions");
  });

  it("renders a badge for git status counts", () => {
    renderSurface({ gitStatusCount: 11 });

    expect(screen.getByRole("button", { name: "Git status" })).toHaveTextContent("11");
  });

  it("renders header actions for the active section", () => {
    renderSurface({
      renderHeaderActions: (sectionId) =>
        sectionId === "sessions" ? <button type="button">New</button> : null,
    });

    expect(screen.getByRole("button", { name: "New" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Files" }));
    expect(screen.queryByRole("button", { name: "New" })).not.toBeInTheDocument();
  });

  it("locks fixed sections without showing the dock or persisting order", () => {
    const ref = createRef<ControlPanelSurfaceHandle>();

    render(
      <ControlPanelSurface
        ref={ref}
        fixedSection="sessions"
        gitStatusCount={5}
        isPreferencesOpen={false}
        onOpenPreferences={() => {}}
        projectCount={3}
        sessionCount={7}
        renderSection={(sectionId) => <div data-testid="section-body">{sectionId}</div>}
      />,
    );

    expect(screen.queryByRole("navigation", { name: "Control panel dock" })).not.toBeInTheDocument();
    expect(document.querySelector(".control-panel-shell")).toHaveClass("fixed-section");
    expect(screen.getByRole("heading", { level: 2, name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("sessions");
    expect(window.localStorage.getItem("termal-control-panel-section-order-v2")).toBeNull();

    act(() => {
      ref.current?.selectSection("git");
    });

    expect(screen.getByRole("heading", { level: 2, name: "Sessions" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { level: 2, name: "Git status" })).not.toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("sessions");
  });

  it("uses Projects, Sessions, Orchestrators, Files, Git status, Board as the default dock order", () => {
    renderSurface();

    expect(getDockSectionLabels()).toEqual([
      "Projects",
      "Sessions",
      "Orchestrators",
      "Files",
      "Git status",
      "Board",
    ]);
  });

  it("ignores the old dock-order key", () => {
    window.localStorage.setItem(
      "termal-control-panel-section-order",
      JSON.stringify(["projects", "sessions", "git", "files"]),
    );

    renderSurface();

    expect(getDockSectionLabels()).toEqual([
      "Projects",
      "Sessions",
      "Orchestrators",
      "Files",
      "Git status",
      "Board",
    ]);
  });

  it("reorders the dock sections by drag and drop and restores that order on remount", () => {
    const { unmount } = renderSurface();
    const projectsButton = screen.getByRole("button", { name: "Projects" });
    const gitButton = screen.getByRole("button", { name: "Git status" });
    const dataTransfer = createDataTransfer();

    mockButtonBounds(projectsButton, { top: 0, height: 40 });

    fireEvent.dragStart(gitButton, { dataTransfer, shiftKey: true });
    fireEvent.dragOver(projectsButton, { clientY: 36, dataTransfer, shiftKey: true });
    fireEvent.drop(projectsButton, { clientY: 36, dataTransfer, shiftKey: true });
    fireEvent.dragEnd(gitButton, { dataTransfer, shiftKey: true });

    expect(getDockSectionLabels()).toEqual([
      "Projects",
      "Git status",
      "Sessions",
      "Orchestrators",
      "Files",
      "Board",
    ]);

    unmount();
    renderSurface();

    expect(getDockSectionLabels()).toEqual([
      "Projects",
      "Git status",
      "Sessions",
      "Orchestrators",
      "Files",
      "Board",
    ]);
  });

  it("drags Board as an internal reorder when it has no launcher tab", () => {
    const onSectionTabDragStart = vi.fn();
    renderSurface({ onSectionTabDragStart });
    const projectsButton = screen.getByRole("button", { name: "Projects" });
    const boardButton = screen.getByRole("button", { name: "Board" });
    const dataTransfer = createDataTransfer();

    expect(boardButton).toHaveAttribute("title", "Board (drag to reorder)");
    mockButtonBounds(projectsButton, { top: 0, height: 40 });
    fireEvent.dragStart(boardButton, { dataTransfer });
    fireEvent.dragOver(projectsButton, { clientY: 36, dataTransfer });
    fireEvent.drop(projectsButton, { clientY: 36, dataTransfer });
    fireEvent.dragEnd(boardButton, { dataTransfer });

    expect(onSectionTabDragStart).not.toHaveBeenCalled();
    expect(getDockSectionLabels()).toEqual([
      "Projects",
      "Board",
      "Sessions",
      "Orchestrators",
      "Files",
      "Git status",
    ]);
  });

  it("uses the external tab drag path for sections with launcher tabs", () => {
    const onSectionTabDragStart = vi.fn();
    renderSurface({ onSectionTabDragStart });
    const projectsButton = screen.getByRole("button", { name: "Projects" });
    const dataTransfer = createDataTransfer();

    expect(projectsButton).toHaveAttribute(
      "title",
      "Projects (drag to open as tab, Shift+drag to reorder)",
    );
    fireEvent.dragStart(projectsButton, { dataTransfer });

    expect(onSectionTabDragStart).toHaveBeenCalledTimes(1);
    expect(onSectionTabDragStart.mock.calls[0]?.[1]).toBe("projects");
  });
});

function renderSurface(
  overrides: Partial<React.ComponentProps<typeof ControlPanelSurface>> = {},
) {
  const launcherOptions = {
    filesystemRoot: "/tmp",
    gitWorkdir: "/tmp",
    originProjectId: "project-1",
    originSessionId: "session-1",
  };
  const sectionLauncherTabs = {
    projects: createControlPanelSectionLauncherTab(
      "projects",
      launcherOptions,
    ),
    sessions: createControlPanelSectionLauncherTab(
      "sessions",
      launcherOptions,
    ),
    orchestrators: createControlPanelSectionLauncherTab(
      "orchestrators",
      launcherOptions,
    ),
    files: createControlPanelSectionLauncherTab("files", launcherOptions),
    git: createControlPanelSectionLauncherTab("git", launcherOptions),
    board: createControlPanelSectionLauncherTab("board", launcherOptions),
  };
  return render(
    <ControlPanelSurface
      gitStatusCount={5}
      isPreferencesOpen={false}
      launcherPaneId="pane-1"
      onOpenPreferences={() => {}}
      projectCount={3}
      sessionCount={7}
      renderSection={(sectionId) => <div data-testid="section-body">{sectionId}</div>}
      sectionLauncherTabs={sectionLauncherTabs}
      windowId="window-1"
      {...overrides}
    />,
  );
}

function getDockSectionLabels() {
  const dock = screen.getByRole("navigation", { name: "Control panel dock" });
  return within(dock)
    .getAllByRole("button")
    .map((button) => button.getAttribute("aria-label"))
    .filter((label): label is string => label !== null && label !== "Open preferences");
}

function createDataTransfer() {
  const data = new Map<string, string>();
  return {
    dropEffect: "move",
    effectAllowed: "move",
    getData: (format: string) => data.get(format) ?? "",
    setData: (format: string, value: string) => {
      data.set(format, value);
    },
  };
}

function mockButtonBounds(button: HTMLElement, bounds: { top: number; height: number }) {
  Object.defineProperty(button, "getBoundingClientRect", {
    configurable: true,
    value: () => ({
      top: bounds.top,
      bottom: bounds.top + bounds.height,
      left: 0,
      right: 40,
      width: 40,
      height: bounds.height,
      x: 0,
      y: bounds.top,
      toJSON: () => ({}),
    }),
  });
}
