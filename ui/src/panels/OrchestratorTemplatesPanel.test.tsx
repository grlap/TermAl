import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  createOrchestratorInstance,
  createOrchestratorTemplate,
  deleteOrchestratorTemplate,
  fetchOrchestratorTemplates,
  updateOrchestratorTemplate,
} from "../api";
import type {
  Project,
  OrchestratorSessionTemplate,
  OrchestratorTemplate,
  OrchestratorTemplateTransition,
} from "../types";
import { OrchestratorTemplatesPanel } from "./OrchestratorTemplatesPanel";

vi.mock("../api", () => ({
  createOrchestratorInstance: vi.fn(),
  createOrchestratorTemplate: vi.fn(),
  deleteOrchestratorTemplate: vi.fn(),
  fetchOrchestratorTemplates: vi.fn(),
  updateOrchestratorTemplate: vi.fn(),
}));

const fetchTemplatesMock = vi.mocked(fetchOrchestratorTemplates);
const createOrchestratorInstanceMock = vi.mocked(createOrchestratorInstance);
const createTemplateMock = vi.mocked(createOrchestratorTemplate);
const updateTemplateMock = vi.mocked(updateOrchestratorTemplate);
const deleteTemplateMock = vi.mocked(deleteOrchestratorTemplate);

describe("OrchestratorTemplatesPanel", () => {
  beforeEach(() => {
    window.localStorage.clear();
    fetchTemplatesMock.mockReset();
    createOrchestratorInstanceMock.mockReset();
    createTemplateMock.mockReset();
    updateTemplateMock.mockReset();
    deleteTemplateMock.mockReset();
  });

  it("loads and displays existing templates", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "orchestrator-template-1",
          name: "Feature Delivery Flow",
          description: "Coordinate implementation and review.",
          sessions: [
            makeSession({ id: "builder", name: "Builder", model: "gpt-5" }),
          ],
        }),
      ],
    });

    render(<OrchestratorTemplatesPanel />);

    expect(
      await screen.findByText("Feature Delivery Flow"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Template name")).toHaveValue(
      "Feature Delivery Flow",
    );
    expect(
      screen.getByLabelText("Name", { selector: "input#session-name-builder" }),
    ).toHaveValue("Builder");
  });

  it("creates a new template from the editor draft", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    createTemplateMock.mockResolvedValue({
      template: makeTemplate({
        id: "orchestrator-template-1",
        name: "Launch Flow",
        projectId: "project-a",
        sessions: [makeSession({ id: "session-1", name: "Session 1" })],
      }),
    });

    render(<OrchestratorTemplatesPanel projects={[makeProject()]} />);

    await screen.findByText(
      "No orchestration templates yet. Start a new one and save it here.",
    );

    fireEvent.change(screen.getByLabelText("Template name"), {
      target: { value: "Launch Flow" },
    });
    fireEvent.change(screen.getByLabelText("Project"), {
      target: { value: "project-a" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add session" }));
    fireEvent.click(screen.getByRole("button", { name: "Create template" }));

    await waitFor(() => {
      expect(createTemplateMock).toHaveBeenCalledWith(
        expect.objectContaining({
          name: "Launch Flow",
          projectId: "project-a",
          sessions: [
            expect.objectContaining({
              id: "session-1",
              name: "Session 1",
            }),
          ],
        }),
      );
    });
    expect(screen.getByText("Template created.")).toBeInTheDocument();
  });

  it("restores the selected project from persisted draft state", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-test",
      JSON.stringify({
        draft: {
          name: "Recovered Flow",
          description: "",
          projectId: "project-a",
          sessions: [makeSession({ id: "session-1", name: "Session 1" })],
          transitions: [],
        },
        selectedNodeId: "session-1",
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-test"
        projects={[makeProject()]}
      />,
    );

    expect(
      await screen.findByDisplayValue("Recovered Flow"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Project")).toHaveValue("project-a");
  });

  it("shows a validation error for cyclic transition drafts", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-cycle",
      JSON.stringify({
        draft: {
          name: "Cycle Flow",
          description: "",
          projectId: null,
          sessions: [
            makeSession({ id: "a", name: "Session A" }),
            makeSession({ id: "b", name: "Session B" }),
          ],
          transitions: [
            makeTransition({
              id: "a-to-b",
              fromSessionId: "a",
              toSessionId: "b",
            }),
            makeTransition({
              id: "b-to-a",
              fromSessionId: "b",
              toSessionId: "a",
            }),
          ],
        },
        selectedNodeId: "a",
        selectedTemplateId: null,
      }),
    );

    render(<OrchestratorTemplatesPanel persistenceKey="orchestrator-cycle" />);

    expect(await screen.findByDisplayValue("Cycle Flow")).toBeInTheDocument();
    expect(
      screen.getByText("Transitions must form a directed acyclic graph."),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Create template" }),
    ).toBeEnabled();
  });

  it("shows a validation error when a draft exceeds the session limit", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-too-many-sessions",
      JSON.stringify({
        draft: {
          name: "Oversized Flow",
          description: "",
          projectId: null,
          sessions: makeManySessions(51),
          transitions: [],
        },
        selectedNodeId: "session-1",
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-too-many-sessions" />,
    );

    expect(
      await screen.findByDisplayValue("Oversized Flow"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Orchestrator templates support at most 50 sessions."),
    ).toBeInTheDocument();
  });

  it("shows a validation error when a draft exceeds the transition limit", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-too-many-transitions",
      JSON.stringify({
        draft: {
          name: "Transition Heavy Flow",
          description: "",
          projectId: null,
          sessions: [
            makeSession({ id: "builder", name: "Builder" }),
            makeSession({
              id: "reviewer",
              name: "Reviewer",
              position: { x: 540, y: 420 },
            }),
          ],
          transitions: makeManyTransitions(201),
        },
        selectedNodeId: "builder",
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-too-many-transitions" />,
    );

    expect(
      await screen.findByDisplayValue("Transition Heavy Flow"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Orchestrator templates support at most 200 transitions.",
      ),
    ).toBeInTheDocument();
  });

  it("allows the 50-session boundary and disables adding another session", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-session-boundary",
      JSON.stringify({
        draft: {
          name: "Boundary Flow",
          description: "",
          projectId: null,
          sessions: makeManySessions(50),
          transitions: [],
        },
        selectedNodeId: "session-1",
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-session-boundary" />,
    );

    expect(
      await screen.findByDisplayValue("Boundary Flow"),
    ).toBeInTheDocument();
    const boardSurface = document.querySelector(".orchestrator-board-surface");
    if (!(boardSurface instanceof HTMLElement)) {
      throw new Error("orchestrator board surface not found");
    }
    fireEvent.click(boardSurface);

    const addSessionButton = screen.getByRole("button", {
      name: "Add session",
    });
    expect(addSessionButton).toBeDisabled();
    expect(addSessionButton).toHaveAttribute(
      "title",
      "Orchestrator templates support at most 50 sessions.",
    );
    expect(
      screen.queryByText("Orchestrator templates support at most 50 sessions."),
    ).not.toBeInTheDocument();
  });

  it("disables Run when the selected template points at a missing project", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-stale-project",
          name: "Stale Project Flow",
          projectId: "project-missing",
          sessions: [makeSession({ id: "builder", name: "Builder" })],
        }),
      ],
    });

    render(
      <OrchestratorTemplatesPanel
        initialTemplateId="template-stale-project"
        projects={[makeProject()]}
      />,
    );

    expect(
      await screen.findByDisplayValue("Stale Project Flow"),
    ).toBeInTheDocument();
    const runButton = screen.getByRole("button", { name: /run/i });
    expect(runButton).toBeDisabled();
    expect(runButton).toHaveAttribute(
      "title",
      "The selected project is no longer available",
    );
  });

  it("starts a template and forwards the full state snapshot", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-run",
          name: "Run Flow",
          projectId: "project-a",
          sessions: [makeSession({ id: "builder", name: "Builder" })],
        }),
      ],
    });
    createOrchestratorInstanceMock.mockResolvedValue({
      orchestrator: {
        id: "orchestrator-1",
        templateId: "template-run",
        projectId: "project-a",
        templateSnapshot: makeTemplate({
          id: "template-run",
          name: "Run Flow",
          projectId: "project-a",
          sessions: [makeSession({ id: "builder", name: "Builder" })],
        }),
        status: "running",
        sessionInstances: [],
        pendingTransitions: [],
        createdAt: "2026-03-30 09:00:00",
        completedAt: null,
      },
      state: {
        revision: 2,
        projects: [makeProject()],
        sessions: [],
      },
    });
    const onStateUpdated = vi.fn();

    render(
      <OrchestratorTemplatesPanel
        initialTemplateId="template-run"
        onStateUpdated={onStateUpdated}
        projects={[makeProject()]}
      />,
    );

    expect(await screen.findByDisplayValue("Run Flow")).toBeInTheDocument();
    const runButton = screen
      .getAllByRole("button", { name: /run/i })
      .find((candidate) =>
        /Run on /.test(candidate.getAttribute("title") ?? ""),
      );
    if (!runButton) {
      throw new Error("Run button not found");
    }
    fireEvent.click(runButton);

    await waitFor(() => {
      expect(createOrchestratorInstanceMock).toHaveBeenCalledWith(
        "template-run",
        "project-a",
      );
    });
    expect(onStateUpdated).toHaveBeenCalledWith({
      revision: 2,
      projects: [makeProject()],
      sessions: [],
    });
    expect(
      screen.getByText("Orchestration started: orchestrator-1"),
    ).toBeInTheDocument();
  });

  it("starts on a blank draft when requested", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "orchestrator-template-1",
          name: "Existing Flow",
          sessions: [makeSession({ id: "builder", name: "Builder" })],
        }),
      ],
    });

    render(<OrchestratorTemplatesPanel startMode="new" />);

    await screen.findByRole("heading", { level: 3, name: "New template" });

    expect(screen.getByLabelText("Template name")).toHaveValue("");
    expect(screen.queryByDisplayValue("Existing Flow")).not.toBeInTheDocument();
  });

  it("selects a requested template when opened from the library", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-a",
          name: "Flow A",
          sessions: [makeSession({ id: "builder-a", name: "Builder A" })],
        }),
        makeTemplate({
          id: "template-b",
          name: "Flow B",
          updatedAt: "2026-03-26 11:00:00",
          sessions: [makeSession({ id: "builder-b", name: "Builder B" })],
        }),
      ],
    });

    render(<OrchestratorTemplatesPanel initialTemplateId="template-b" />);

    expect(await screen.findByDisplayValue("Flow B")).toBeInTheDocument();
    expect(screen.queryByDisplayValue("Flow A")).not.toBeInTheDocument();
  });

  it("opens a requested template in canvas-focused edit mode", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-b",
          name: "Flow B",
          updatedAt: "2026-03-26 11:00:00",
          sessions: [makeSession({ id: "builder-b", name: "Builder B" })],
        }),
      ],
    });

    render(
      <OrchestratorTemplatesPanel
        initialTemplateId="template-b"
        startMode="edit"
      />,
    );

    expect(await screen.findByDisplayValue("Flow B")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { level: 3, name: "Canvas" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { level: 3, name: "Templates" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "New template" }),
    ).not.toBeInTheDocument();
  });

  it("renders transition note markers on the canvas", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-transition",
          name: "Flow With Transition",
          sessions: [
            makeSession({
              id: "dev",
              name: "Dev",
              position: { x: 140, y: 160 },
            }),
            makeSession({
              id: "reviewer",
              name: "Reviewer",
              position: { x: 760, y: 420 },
            }),
          ],
          transitions: [
            makeTransition({
              id: "transition-1",
              fromSessionId: "dev",
              toSessionId: "reviewer",
            }),
          ],
        }),
      ],
    });

    render(
      <OrchestratorTemplatesPanel
        initialTemplateId="template-transition"
        startMode="edit"
      />,
    );

    expect(
      await screen.findByTitle("transition-1: Dev -> Reviewer"),
    ).toBeInTheDocument();
  });
});

function makeTemplate(
  overrides: Partial<OrchestratorTemplate> = {},
): OrchestratorTemplate {
  return {
    id: "template-1",
    name: "Delivery Flow",
    description: "",
    createdAt: "2026-03-26 10:00:00",
    updatedAt: "2026-03-26 10:00:00",
    sessions: [makeSession()],
    transitions: [],
    ...overrides,
  };
}

function makeSession(
  overrides: Partial<OrchestratorSessionTemplate> = {},
): OrchestratorSessionTemplate {
  return {
    id: "builder",
    name: "Builder",
    agent: "Codex",
    model: null,
    instructions: "Implement the change.",
    autoApprove: true,
    position: { x: 180, y: 420 },
    ...overrides,
  };
}

function makeTransition(
  overrides: Partial<OrchestratorTemplateTransition> = {},
): OrchestratorTemplateTransition {
  return {
    id: "transition-1",
    fromSessionId: "builder",
    toSessionId: "reviewer",
    trigger: "onCompletion",
    resultMode: "lastResponse",
    promptTemplate: "Continue with:\n{{result}}",
    ...overrides,
  };
}

function makeManySessions(count: number): OrchestratorSessionTemplate[] {
  return Array.from({ length: count }, (_, index) =>
    makeSession({
      id: `session-${index + 1}`,
      name: `Session ${index + 1}`,
      position: {
        x: 120 + (index % 5) * 220,
        y: 120 + Math.floor(index / 5) * 180,
      },
    }),
  );
}

function makeManyTransitions(count: number): OrchestratorTemplateTransition[] {
  return Array.from({ length: count }, (_, index) =>
    makeTransition({ id: `transition-${index + 1}` }),
  );
}

function makeProject(overrides: Partial<Project> = {}): Project {
  return {
    id: "project-a",
    name: "Project A",
    rootPath: "/repo",
    remoteId: "local",
    ...overrides,
  };
}
