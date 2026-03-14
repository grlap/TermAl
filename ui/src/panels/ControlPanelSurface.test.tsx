import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";

import { ControlPanelSurface } from "./ControlPanelSurface";

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

    fireEvent.click(screen.getByRole("button", { name: "Git status" }));

    expect(screen.getByRole("heading", { level: 2, name: "Git status" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("git");
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

  it("uses Projects, Sessions, Files, Git status as the default dock order", () => {
    renderSurface();

    expect(getDockSectionLabels()).toEqual(["Projects", "Sessions", "Files", "Git status"]);
  });

  it("migrates the legacy dock order so Files lands directly above Git status", () => {
    window.localStorage.setItem(
      "termal-control-panel-section-order",
      JSON.stringify(["projects", "sessions", "git", "files"]),
    );

    renderSurface();

    expect(getDockSectionLabels()).toEqual(["Projects", "Sessions", "Files", "Git status"]);
  });

  it("reorders the dock sections by drag and drop and restores that order on remount", () => {
    const { unmount } = renderSurface();
    const projectsButton = screen.getByRole("button", { name: "Projects" });
    const gitButton = screen.getByRole("button", { name: "Git status" });
    const dataTransfer = createDataTransfer();

    mockButtonBounds(projectsButton, { top: 0, height: 40 });

    fireEvent.dragStart(gitButton, { dataTransfer });
    fireEvent.dragOver(projectsButton, { clientY: 36, dataTransfer });
    fireEvent.drop(projectsButton, { clientY: 36, dataTransfer });
    fireEvent.dragEnd(gitButton, { dataTransfer });

    expect(getDockSectionLabels()).toEqual(["Projects", "Git status", "Sessions", "Files"]);

    unmount();
    renderSurface();

    expect(getDockSectionLabels()).toEqual(["Projects", "Git status", "Sessions", "Files"]);
  });
});

function renderSurface(
  overrides: Partial<React.ComponentProps<typeof ControlPanelSurface>> = {},
) {
  return render(
    <ControlPanelSurface
      gitStatusCount={5}
      isPreferencesOpen={false}
      onOpenPreferences={() => {}}
      projectCount={3}
      sessionCount={7}
      renderSection={(sectionId) => <div data-testid="section-body">{sectionId}</div>}
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
