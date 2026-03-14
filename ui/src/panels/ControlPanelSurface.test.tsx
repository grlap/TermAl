import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ControlPanelSurface } from "./ControlPanelSurface";

describe("ControlPanelSurface", () => {
  it("switches sections from the activity rail", () => {
    render(
      <ControlPanelSurface
        gitStatusCount={5}
        isPreferencesOpen={false}
        onOpenPreferences={() => {}}
        projectCount={3}
        sessionCount={7}
        renderSection={(sectionId) => <div data-testid="section-body">{sectionId}</div>}
      />,
    );

    expect(screen.getByRole("heading", { level: 2, name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("sessions");

    fireEvent.click(screen.getByRole("button", { name: "Projects" }));

    expect(screen.getByRole("heading", { level: 2, name: "Projects" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("projects");

    fireEvent.click(screen.getByRole("button", { name: "Git status" }));

    expect(screen.getByRole("heading", { level: 2, name: "Git status" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("git");
  });

  it("opens preferences from the dock without switching sections", () => {
    const onOpenPreferences = vi.fn();

    render(
      <ControlPanelSurface
        gitStatusCount={5}
        isPreferencesOpen={false}
        onOpenPreferences={onOpenPreferences}
        projectCount={3}
        sessionCount={7}
        renderSection={(sectionId) => <div data-testid="section-body">{sectionId}</div>}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Open preferences" }));

    expect(onOpenPreferences).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("heading", { level: 2, name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByTestId("section-body")).toHaveTextContent("sessions");
  });

  it("renders a badge for git status counts", () => {
    render(
      <ControlPanelSurface
        gitStatusCount={11}
        isPreferencesOpen={false}
        onOpenPreferences={() => {}}
        projectCount={3}
        sessionCount={7}
        renderSection={(sectionId) => <div data-testid="section-body">{sectionId}</div>}
      />,
    );

    expect(screen.getByRole("button", { name: "Git status" })).toHaveTextContent("11");
  });
});
