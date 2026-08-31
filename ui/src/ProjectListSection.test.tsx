import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ProjectListSection } from "./ProjectListSection";
import type { Project } from "./types";

const project: Project = {
  id: "project-1",
  name: "TermAl",
  rootPath: "C:\\github\\Personal\\TermAl",
  remoteId: "local",
  engramDeclared: true,
  engramGrantConfigured: true,
  engram: {
    enabled: true,
    binaryPath: "engram",
    home: "C:\\Users\\greg\\.engram",
    deadlineMs: 2000,
  },
};

describe("ProjectListSection Engram settings", () => {
  it("shows project Engram status and opens the settings tab from the context menu", () => {
    render(
      <ProjectListSection
        paneId="pane-1"
        projectSessionCounts={new Map([[project.id, 2]])}
        projects={[project]}
        remoteLookup={new Map()}
        selectedProjectId={project.id}
        sessionCount={2}
        onProjectScopeChange={vi.fn()}
        onRemoveProject={vi.fn()}
        onStartSession={vi.fn()}
        onStateUpdated={vi.fn()}
      />,
    );

    expect(screen.getByText("Engram · Enabled")).toBeInTheDocument();

    const projectRow = screen.getByRole("button", { name: /TermAl/ });
    fireEvent.contextMenu(projectRow, { clientX: 40, clientY: 50 });
    fireEvent.click(screen.getByRole("menuitem", { name: "Engram settings" }));

    expect(screen.getByRole("dialog", { name: "TermAl" })).toBeInTheDocument();
    expect(
      screen.getByRole("tab", { name: "Engram", selected: true }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Work authority grant")).toHaveAttribute(
      "type",
      "password",
    );
  });

  it("does not expose Engram controls for an undeclared repository", () => {
    render(
      <ProjectListSection
        paneId="pane-1"
        projectSessionCounts={new Map([[project.id, 2]])}
        projects={[{ ...project, engramDeclared: false }]}
        remoteLookup={new Map()}
        selectedProjectId={project.id}
        sessionCount={2}
        onProjectScopeChange={vi.fn()}
        onRemoveProject={vi.fn()}
        onStartSession={vi.fn()}
        onStateUpdated={vi.fn()}
      />,
    );

    expect(screen.queryByText(/^Engram ·/)).not.toBeInTheDocument();
    fireEvent.contextMenu(screen.getByRole("button", { name: /TermAl/ }), {
      clientX: 40,
      clientY: 50,
    });
    expect(
      screen.queryByRole("menuitem", { name: "Engram settings" }),
    ).not.toBeInTheDocument();
  });
});
