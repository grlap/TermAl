import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  within,
  waitFor,
} from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createOrchestratorInstance,
  createOrchestratorTemplate,
  deleteOrchestratorTemplate,
  fetchOrchestratorTemplates,
  updateOrchestratorTemplate,
  type StateResponse,
} from "../api";
import type {
  Project,
  OrchestratorSessionTemplate,
  OrchestratorTemplate,
  OrchestratorTemplateTransition,
  Session,
} from "../types";
import {
  objectHasOwnWithFallback,
  OrchestratorTemplatesPanel,
} from "./OrchestratorTemplatesPanel";

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

async function selectComboboxOption(
  name: string,
  optionName: string | RegExp,
) {
  fireEvent.click(await screen.findByRole("combobox", { name }));

  const listbox = await screen.findByRole("listbox");
  const option = within(listbox)
    .getAllByRole("option")
    .find((candidate) => {
      const label =
        candidate.querySelector(".combo-option-label")?.textContent?.trim() ??
        candidate.textContent?.trim() ??
        "";

      return typeof optionName === "string"
        ? label === optionName
        : optionName.test(label);
    });

  if (!option) {
    throw new Error(`Combobox option not found for ${String(optionName)}`);
  }

  fireEvent.click(option);
}

function makeStateResponse(
  overrides: Pick<
    StateResponse,
    "revision" | "projects" | "orchestrators" | "workspaces" | "sessions"
  > &
    Partial<Pick<StateResponse, "codex" | "agentReadiness" | "preferences">>,
): StateResponse {
  return {
    revision: overrides.revision,
    codex: overrides.codex ?? {},
    agentReadiness: overrides.agentReadiness ?? [],
    preferences: overrides.preferences ?? {
      defaultCodexReasoningEffort: "medium",
      defaultClaudeApprovalMode: "ask",
      defaultClaudeEffort: "default",
    },
    projects: overrides.projects,
    orchestrators: overrides.orchestrators,
    workspaces: overrides.workspaces,
    sessions: overrides.sessions,
  };
}

describe("objectHasOwnWithFallback", () => {
  it("uses Object.hasOwn when available", () => {
    const allowlist = { Builder: true };

    expect(objectHasOwnWithFallback(allowlist, "Builder")).toBe(true);
    expect(objectHasOwnWithFallback(allowlist, "Missing")).toBe(false);
  });
  it("falls back to hasOwnProperty when Object.hasOwn is unavailable", () => {
    const objectWithHasOwn = Object as ObjectConstructor & {
      hasOwn?: (target: object, key: PropertyKey) => boolean;
    };
    const originalHasOwn = objectWithHasOwn.hasOwn;
    const allowlist = { Builder: true };

    Object.defineProperty(Object, "hasOwn", {
      configurable: true,
      value: undefined,
      writable: true,
    });

    try {
      expect(objectHasOwnWithFallback(allowlist, "Builder")).toBe(true);
      expect(objectHasOwnWithFallback(allowlist, "Missing")).toBe(false);
    } finally {
      Object.defineProperty(Object, "hasOwn", {
        configurable: true,
        value: originalHasOwn,
        writable: true,
      });
    }
  });
});

describe("OrchestratorTemplatesPanel", () => {
  beforeEach(() => {
    window.localStorage.clear();
    fetchTemplatesMock.mockReset();
    createOrchestratorInstanceMock.mockReset();
    createTemplateMock.mockReset();
    updateTemplateMock.mockReset();
    deleteTemplateMock.mockReset();
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
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
              inputMode: "queue",
            }),
          ],
        }),
      );
    });
    expect(screen.getByText("Template created.")).toBeInTheDocument();
  });

  it("saves a session input mode override on the template draft", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    createTemplateMock.mockResolvedValue({
      template: makeTemplate({
        id: "orchestrator-template-2",
        name: "Consolidation Flow",
        sessions: [
          makeSession({
            id: "session-1",
            name: "Session 1",
            inputMode: "consolidate",
          }),
        ],
      }),
    });

    render(<OrchestratorTemplatesPanel />);

    await screen.findByText(
      "No orchestration templates yet. Start a new one and save it here.",
    );

    fireEvent.change(screen.getByLabelText("Template name"), {
      target: { value: "Consolidation Flow" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add session" }));
    fireEvent.change(screen.getByLabelText("Incoming transitions"), {
      target: { value: "consolidate" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create template" }));

    await waitFor(() => {
      expect(createTemplateMock).toHaveBeenCalledWith(
        expect.objectContaining({
          name: "Consolidation Flow",
          sessions: [
            expect.objectContaining({
              id: "session-1",
              inputMode: "consolidate",
            }),
          ],
        }),
      );
    });
  });

  it("saves a session model override from the template draft", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    createTemplateMock.mockResolvedValue({
      template: makeTemplate({
        id: "orchestrator-template-model",
        name: "Model Flow",
        sessions: [
          makeSession({
            id: "session-1",
            name: "Session 1",
            model: "gpt-5.4",
          }),
        ],
      }),
    });

    render(<OrchestratorTemplatesPanel />);

    await screen.findByText(
      "No orchestration templates yet. Start a new one and save it here.",
    );

    fireEvent.change(screen.getByLabelText("Template name"), {
      target: { value: "Model Flow" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add session" }));
    await selectComboboxOption("Model", "GPT-5.4");
    fireEvent.click(screen.getByRole("button", { name: "Create template" }));

    await waitFor(() => {
      expect(createTemplateMock).toHaveBeenCalledWith(
        expect.objectContaining({
          name: "Model Flow",
          sessions: [
            expect.objectContaining({
              id: "session-1",
              model: "gpt-5.4",
            }),
          ],
        }),
      );
    });
  });

  it("offers live model options from known sessions", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    createTemplateMock.mockResolvedValue({
      template: makeTemplate({
        id: "orchestrator-template-live-model",
        name: "Live Model Flow",
        sessions: [
          makeSession({
            id: "session-1",
            name: "Session 1",
            model: "gpt-5.5",
          }),
        ],
      }),
    });

    render(
      <OrchestratorTemplatesPanel
        sessions={[
          makeRuntimeSession({
            agent: "Codex",
            modelOptions: [
              {
                label: "GPT-5.5",
                value: "gpt-5.5",
                description: "Latest Codex model",
                badges: ["Preview"],
              },
            ],
          }),
        ]}
      />,
    );

    await screen.findByText(
      "No orchestration templates yet. Start a new one and save it here.",
    );

    fireEvent.change(screen.getByLabelText("Template name"), {
      target: { value: "Live Model Flow" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add session" }));
    await selectComboboxOption("Model", "GPT-5.5");
    fireEvent.click(screen.getByRole("button", { name: "Create template" }));

    await waitFor(() => {
      expect(createTemplateMock).toHaveBeenCalledWith(
        expect.objectContaining({
          name: "Live Model Flow",
          sessions: [
            expect.objectContaining({
              id: "session-1",
              model: "gpt-5.5",
            }),
          ],
        }),
      );
    });
  });

  it("resets the session model when the agent changes", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    createTemplateMock.mockResolvedValue({
      template: makeTemplate({
        id: "orchestrator-template-agent-reset",
        name: "Agent Reset Flow",
        sessions: [
          makeSession({
            id: "session-1",
            name: "Session 1",
            agent: "Claude",
            model: "",
          }),
        ],
      }),
    });

    render(<OrchestratorTemplatesPanel />);

    await screen.findByText(
      "No orchestration templates yet. Start a new one and save it here.",
    );

    fireEvent.change(screen.getByLabelText("Template name"), {
      target: { value: "Agent Reset Flow" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add session" }));
    await selectComboboxOption("Model", "GPT-5.4");
    await selectComboboxOption("Agent", "Claude");
    fireEvent.click(screen.getByRole("button", { name: "Create template" }));

    await waitFor(() => {
      expect(createTemplateMock).toHaveBeenCalledWith(
        expect.objectContaining({
          name: "Agent Reset Flow",
          sessions: [
            expect.objectContaining({
              id: "session-1",
              agent: "Claude",
              model: "",
            }),
          ],
        }),
      );
    });
  });

  it("shows only one Default option for Claude session models", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });

    render(<OrchestratorTemplatesPanel />);

    await screen.findByText(
      "No orchestration templates yet. Start a new one and save it here.",
    );

    fireEvent.click(screen.getByRole("button", { name: "Add session" }));
    await selectComboboxOption("Agent", "Claude");
    fireEvent.click(await screen.findByRole("combobox", { name: "Model" }));

    const listbox = await screen.findByRole("listbox");
    const defaultOptions = within(listbox)
      .getAllByRole("option")
      .filter((option) => {
        const label =
          option.querySelector(".combo-option-label")?.textContent?.trim() ??
          option.textContent?.trim() ??
          "";
        return label === "Default";
      });

    expect(defaultOptions).toHaveLength(1);
  });

  it("clears dirty state after saving an existing template", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-existing",
          name: "Codium",
          description: "Code factory",
          projectId: "project-a",
          sessions: [makeSession({ id: "session-1", name: "Entry" })],
        }),
      ],
    });
    updateTemplateMock.mockResolvedValue({
      template: makeTemplate({
        id: "template-existing",
        name: "Codium",
        description: "Code factory updated",
        projectId: "project-a",
        sessions: [makeSession({ id: "session-1", name: "Entry" })],
      }),
    });

    render(<OrchestratorTemplatesPanel projects={[makeProject()]} />);

    await screen.findByDisplayValue("Codium");

    fireEvent.change(screen.getByLabelText("Description"), {
      target: { value: "Code factory updated" },
    });

    const saveButton = screen.getByRole("button", { name: "Save template" });
    expect(saveButton).toBeEnabled();

    fireEvent.click(saveButton);

    await waitFor(() => {
      expect(updateTemplateMock).toHaveBeenCalledWith(
        "template-existing",
        expect.objectContaining({
          description: "Code factory updated",
        }),
      );
    });

    await waitFor(() => {
      expect(screen.getByText("Template saved.")).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Save template" }),
      ).toBeDisabled();
    });

    const runButton = screen.getAllByRole("button", { name: /run/i })[0];
    expect(runButton).toBeEnabled();
  });

  it("deletes the selected template and selects the next available template", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-delete",
          name: "Delete Me",
          sessions: [
            makeSession({ id: "builder-delete", name: "Builder Delete" }),
          ],
        }),
        makeTemplate({
          id: "template-keep",
          name: "Keep Me",
          updatedAt: "2026-03-26 11:00:00",
          sessions: [makeSession({ id: "builder-keep", name: "Builder Keep" })],
        }),
      ],
    });
    deleteTemplateMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-keep",
          name: "Keep Me",
          updatedAt: "2026-03-26 11:00:00",
          sessions: [makeSession({ id: "builder-keep", name: "Builder Keep" })],
        }),
      ],
    });

    render(<OrchestratorTemplatesPanel initialTemplateId="template-delete" />);

    expect(await screen.findByDisplayValue("Delete Me")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));

    await waitFor(() => {
      expect(deleteTemplateMock).toHaveBeenCalledWith("template-delete");
    });
    await waitFor(() => {
      expect(screen.getByText("Template deleted.")).toBeInTheDocument();
      expect(screen.getByLabelText("Template name")).toHaveValue("Keep Me");
    });
    expect(
      screen.getByLabelText("Name", {
        selector: "input#session-name-builder-keep",
      }),
    ).toHaveValue("Builder Keep");
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

  it("keeps restored template edits dirty against the saved template after reload", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-recovered",
          name: "Saved Flow",
          projectId: "project-a",
          sessions: [makeSession({ id: "builder", name: "Builder" })],
        }),
      ],
    });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-dirty-template",
      JSON.stringify({
        draft: {
          name: "Recovered Flow",
          description: "Recovered local edits",
          projectId: "project-a",
          sessions: [makeSession({ id: "builder", name: "Builder" })],
          transitions: [],
        },
        selectedNodeId: "builder",
        selectedTemplateId: "template-recovered",
      }),
    );

    render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-dirty-template"
        projects={[makeProject()]}
      />,
    );

    expect(
      await screen.findByDisplayValue("Recovered Flow"),
    ).toBeInTheDocument();

    const saveButton = screen.getByRole("button", { name: "Save template" });
    expect(saveButton).toBeEnabled();

    const runButton = screen
      .getAllByRole("button", { name: /run/i })
      .find((candidate) =>
        /Save changes before running/.test(
          candidate.getAttribute("title") ?? "",
        ),
      );
    if (!runButton) {
      throw new Error("Run button not found");
    }
    expect(runButton).toBeDisabled();
    expect(runButton).toHaveAttribute("title", "Save changes before running");

    fireEvent.click(screen.getByRole("button", { name: "Reset draft" }));

    await waitFor(() => {
      expect(screen.getByLabelText("Template name")).toHaveValue("Saved Flow");
    });
    expect(
      screen.getByRole("button", { name: "Save template" }),
    ).toBeDisabled();
    const refreshedRunButton = screen
      .getAllByRole("button", { name: /run/i })
      .find((candidate) =>
        /Run on /.test(candidate.getAttribute("title") ?? ""),
      );
    if (!refreshedRunButton) {
      throw new Error("Run button not found after reset");
    }
    expect(refreshedRunButton).toBeEnabled();
  });

  it("ignores restored drafts with a missing input mode", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    const invalidSession = {
      ...makeSession({ id: "builder", name: "Builder" }),
    } as Record<string, unknown>;
    delete invalidSession.inputMode;
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-missing-input-mode",
      JSON.stringify({
        draft: {
          name: "Invalid InputMode Flow",
          description: "",
          projectId: null,
          sessions: [invalidSession],
          transitions: [],
        },
        selectedNodeId: "builder",
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-missing-input-mode" />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Template name")).toHaveValue("");
    });
    expect(screen.queryByDisplayValue("Invalid InputMode Flow")).not.toBeInTheDocument();
  });

  it("reopens restored edits as a new draft when the selected template no longer exists", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [makeTemplate({ id: "template-live", name: "Live Flow" })],
    });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-missing-template",
      JSON.stringify({
        draft: {
          name: "Recovered Flow",
          description: "Recovered local edits",
          projectId: null,
          sessions: [makeSession({ id: "builder", name: "Builder" })],
          transitions: [],
        },
        selectedNodeId: "builder",
        selectedTemplateId: "template-missing",
      }),
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-missing-template" />,
    );

    expect(await screen.findByText("Live Flow")).toBeInTheDocument();
    expect(screen.getByLabelText("Template name")).toHaveValue(
      "Recovered Flow",
    );
    expect(
      screen.getByRole("button", { name: "Create template" }),
    ).toBeEnabled();
  });

  it("ignores restored drafts with a null input mode", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-null-input-mode",
      JSON.stringify({
        draft: {
          name: "Invalid InputMode Flow",
          description: "",
          projectId: null,
          sessions: [
            {
              ...makeSession({ id: "builder", name: "Builder" }),
              inputMode: null,
            },
          ],
          transitions: [],
        },
        selectedNodeId: "builder",
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-null-input-mode" />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Template name")).toHaveValue("");
    });
    expect(screen.queryByDisplayValue("Invalid InputMode Flow")).not.toBeInTheDocument();
  });

  it("ignores restored drafts with an invalid model type", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    const invalidSession = {
      ...makeSession({ id: "builder", name: "Builder" }),
      model: 42,
    } as Record<string, unknown>;
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-invalid-model",
      JSON.stringify({
        draft: {
          name: "Invalid Model Flow",
          description: "",
          projectId: null,
          sessions: [invalidSession],
          transitions: [],
        },
        selectedNodeId: "builder",
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-invalid-model" />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Template name")).toHaveValue("");
    });
    expect(screen.queryByDisplayValue("Invalid Model Flow")).not.toBeInTheDocument();
  });

  it("ignores restored drafts with an invalid agent value", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    const invalidSession = {
      ...makeSession({ id: "builder", name: "Builder" }),
      agent: "Cluade",
    } as Record<string, unknown>;
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-invalid-agent",
      JSON.stringify({
        draft: {
          name: "Invalid Agent Flow",
          description: "",
          projectId: null,
          sessions: [invalidSession],
          transitions: [],
        },
        selectedNodeId: "builder",
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-invalid-agent" />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Template name")).toHaveValue("");
    });
    expect(screen.queryByDisplayValue("Invalid Agent Flow")).not.toBeInTheDocument();
  });

  it("ignores restored drafts with a non-finite session position", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-invalid-position",
      '{"draft":{"name":"Invalid Position Flow","description":"","projectId":null,"sessions":[{"id":"builder","name":"Builder","agent":"Codex","model":"gpt-5","instructions":"","autoApprove":false,"inputMode":"queue","position":{"x":1e309,"y":160}}],"transitions":[]},"selectedNodeId":"builder","selectedTemplateId":null}',
    );

    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-invalid-position" />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Template name")).toHaveValue("");
    });
    expect(
      screen.queryByDisplayValue("Invalid Position Flow"),
    ).not.toBeInTheDocument();
  });

  it("clamps restored drafts with out-of-bounds session positions", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    const stateKey =
      "termal-orchestrator-panel-state:orchestrator-clamped-position";
    window.localStorage.setItem(
      stateKey,
      JSON.stringify({
        draft: {
          name: "Clamped Position Flow",
          description: "",
          projectId: null,
          sessions: [
            makeSession({
              id: "builder",
              name: "Builder",
              position: { x: -100, y: 9999 },
            }),
          ],
          transitions: [],
        },
        selectedNodeId: "builder",
        selectedTemplateId: null,
      }),
    );

    const { container } = render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-clamped-position"
        startMode="edit"
      />,
    );

    expect(
      await screen.findByDisplayValue("Clamped Position Flow"),
    ).toBeInTheDocument();

    const card = container.querySelector(".orchestrator-board-card");
    if (!(card instanceof HTMLElement)) {
      throw new Error("Expected a restored session card");
    }

    expect(card.style.left).toBe("32px");
    expect(card.style.top).toBe("1392px");

    act(() => {
      window.dispatchEvent(new Event("pagehide"));
    });

    expect(window.localStorage.getItem(stateKey)).toContain('"x":32');
    expect(window.localStorage.getItem(stateKey)).toContain('"y":1392');
  });

  it("ignores restored drafts with an invalid transition prompt template", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    const invalidTransition = {
      ...makeTransition(),
      promptTemplate: {},
    } as Record<string, unknown>;
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-invalid-transition-prompt-template",
      JSON.stringify({
        draft: {
          name: "Invalid Transition Prompt Flow",
          description: "",
          projectId: null,
          sessions: [
            makeSession({ id: "builder", name: "Builder" }),
            makeSession({
              id: "reviewer",
              name: "Reviewer",
              position: { x: 520, y: 420 },
            }),
          ],
          transitions: [invalidTransition],
        },
        selectedNodeId: null,
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-invalid-transition-prompt-template"
      />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Template name")).toHaveValue("");
    });
    expect(
      screen.queryByDisplayValue("Invalid Transition Prompt Flow"),
    ).not.toBeInTheDocument();
  });

  it("ignores restored drafts with an invalid transition anchor", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    const invalidTransition = {
      ...makeTransition(),
      fromAnchor: { side: "left" },
    } as Record<string, unknown>;
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-invalid-transition-anchor",
      JSON.stringify({
        draft: {
          name: "Invalid Transition Anchor Flow",
          description: "",
          projectId: null,
          sessions: [
            makeSession({ id: "builder", name: "Builder" }),
            makeSession({
              id: "reviewer",
              name: "Reviewer",
              position: { x: 520, y: 420 },
            }),
          ],
          transitions: [invalidTransition],
        },
        selectedNodeId: null,
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-invalid-transition-anchor"
      />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Template name")).toHaveValue("");
    });
    expect(
      screen.queryByDisplayValue("Invalid Transition Anchor Flow"),
    ).not.toBeInTheDocument();
  });

  it("normalizes restored null transition anchors before persisting the draft", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    const stateKey =
      "termal-orchestrator-panel-state:orchestrator-null-transition-anchor";
    const restoredTransition = {
      ...makeTransition(),
      fromAnchor: null,
      toAnchor: null,
    } as Record<string, unknown>;
    window.localStorage.setItem(
      stateKey,
      JSON.stringify({
        draft: {
          name: "Null Transition Anchor Flow",
          description: "",
          projectId: null,
          sessions: [
            makeSession({ id: "builder", name: "Builder" }),
            makeSession({
              id: "reviewer",
              name: "Reviewer",
              position: { x: 520, y: 420 },
            }),
          ],
          transitions: [restoredTransition],
        },
        selectedNodeId: null,
        selectedTemplateId: null,
      }),
    );

    render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-null-transition-anchor"
      />,
    );

    expect(
      await screen.findByDisplayValue("Null Transition Anchor Flow"),
    ).toBeInTheDocument();

    act(() => {
      window.dispatchEvent(new Event("pagehide"));
    });

    const persistedState = window.localStorage.getItem(stateKey);
    expect(persistedState).not.toBeNull();
    const parsedState = JSON.parse(persistedState as string) as {
      draft: {
        transitions: Record<string, unknown>[];
      };
    };
    expect(parsedState.draft.transitions).toHaveLength(1);
    expect(parsedState.draft.transitions[0]).toMatchObject({
      id: "transition-1",
      fromSessionId: "builder",
      toSessionId: "reviewer",
    });
    expect(parsedState.draft.transitions[0]).not.toHaveProperty("fromAnchor");
    expect(parsedState.draft.transitions[0]).not.toHaveProperty("toAnchor");
  });

  it("keeps an identical restored draft clean against the server snapshot", async () => {
    fetchTemplatesMock.mockResolvedValue({
      templates: [
        makeTemplate({
          id: "template-clean",
          name: "Saved Flow",
          projectId: "project-a",
          sessions: [makeSession({ id: "builder", name: "Builder" })],
        }),
      ],
    });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-clean-template",
      JSON.stringify({
        draft: {
          name: "Saved Flow",
          description: "",
          projectId: "project-a",
          sessions: [makeSession({ id: "builder", name: "Builder" })],
          transitions: [],
        },
        selectedNodeId: "builder",
        selectedTemplateId: "template-clean",
      }),
    );

    render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-clean-template"
        projects={[makeProject()]}
      />,
    );

    expect(await screen.findByDisplayValue("Saved Flow")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Save template" }),
    ).toBeDisabled();
    const runButton = screen
      .getAllByRole("button", { name: /run/i })
      .find((candidate) =>
        /Run on Project A/.test(candidate.getAttribute("title") ?? ""),
      );
    if (!runButton) {
      throw new Error("Run button not found");
    }
    expect(runButton).toBeEnabled();
  });

  it("keeps a saved template clean when the panel unmounts before the persistence debounce fires", async () => {
    const initialTemplate = makeTemplate({
      id: "template-clean",
      name: "Codium",
      projectId: "project-a",
      sessions: [makeSession({ id: "builder", name: "Builder" })],
    });
    const savedTemplate = makeTemplate({
      id: "template-clean",
      name: "Codium",
      description: "Code factory",
      projectId: "project-a",
      sessions: [makeSession({ id: "builder", name: "Builder" })],
      updatedAt: "2026-04-01 12:00:00",
    });
    const stateKey = "termal-orchestrator-panel-state:orchestrator-save-unmount";

    fetchTemplatesMock.mockResolvedValueOnce({ templates: [initialTemplate] });
    fetchTemplatesMock.mockResolvedValueOnce({ templates: [savedTemplate] });
    updateTemplateMock.mockResolvedValue({ template: savedTemplate });

    const { unmount } = render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-save-unmount"
        projects={[makeProject()]}
      />,
    );

    expect(await screen.findByDisplayValue("Codium")).toBeInTheDocument();

    vi.useFakeTimers();
    try {
      fireEvent.change(screen.getByLabelText("Description"), {
        target: { value: "Code factory" },
      });

      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Save template" }));
        await Promise.resolve();
      });

      expect(updateTemplateMock).toHaveBeenCalledWith(
        "template-clean",
        expect.objectContaining({
          description: "Code factory",
          projectId: "project-a",
        }),
      );
      expect(screen.getByText("Template saved.")).toBeInTheDocument();
      expect(window.localStorage.getItem(stateKey)).toContain(
        '"description":"Code factory"',
      );

      unmount();
    } finally {
      vi.useRealTimers();
    }

    render(
      <OrchestratorTemplatesPanel
        persistenceKey="orchestrator-save-unmount"
        projects={[makeProject()]}
      />,
    );

    expect(await screen.findByDisplayValue("Codium")).toBeInTheDocument();
    expect(screen.getByLabelText("Description")).toHaveValue("Code factory");
    expect(
      screen.getByRole("button", { name: "Save template" }),
    ).toBeDisabled();
    const runButton = screen
      .getAllByRole("button", { name: /run/i })
      .find((candidate) =>
        /Run on Project A/.test(candidate.getAttribute("title") ?? ""),
      );
    if (!runButton) {
      throw new Error("Run button not found after remount");
    }
    expect(runButton).toBeEnabled();
  });

  it("clears Saving and re-enables Run after save in StrictMode", async () => {
    const initialTemplate = makeTemplate({
      id: "template-strict",
      name: "Codium",
      projectId: "project-a",
      sessions: [makeSession({ id: "builder", name: "Builder" })],
    });
    const savedTemplate = makeTemplate({
      id: "template-strict",
      name: "Codium",
      description: "Code factory",
      projectId: "project-a",
      sessions: [makeSession({ id: "builder", name: "Builder" })],
      updatedAt: "2026-04-01 12:30:00",
    });

    fetchTemplatesMock.mockResolvedValue({ templates: [initialTemplate] });
    updateTemplateMock.mockResolvedValue({ template: savedTemplate });

    render(
      <StrictMode>
        <OrchestratorTemplatesPanel projects={[makeProject()]} />
      </StrictMode>,
    );

    expect(await screen.findByDisplayValue("Codium")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Description"), {
      target: { value: "Code factory" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save template" }));

    await waitFor(() => {
      expect(updateTemplateMock).toHaveBeenCalledWith(
        "template-strict",
        expect.objectContaining({ description: "Code factory" }),
      );
    });
    expect(await screen.findByText("Template saved.")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Save template" }),
    ).toBeDisabled();
    const runButton = screen
      .getAllByRole("button", { name: /run/i })
      .find((candidate) =>
        /Run on Project A/.test(candidate.getAttribute("title") ?? ""),
      );
    if (!runButton) {
      throw new Error("Run button not found in StrictMode");
    }
    expect(runButton).toBeEnabled();
  });

  it("flushes pending draft persistence on pagehide before the debounce fires", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-pagehide" />,
    );

    expect(
      await screen.findByText(
        "No orchestration templates yet. Start a new one and save it here.",
      ),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Template name"), {
      target: { value: "Unsaved Flow" },
    });

    const stateKey = "termal-orchestrator-panel-state:orchestrator-pagehide";
    expect(window.localStorage.getItem(stateKey)).toBeNull();

    act(() => {
      window.dispatchEvent(new Event("pagehide"));
    });

    expect(window.localStorage.getItem(stateKey)).toContain(
      '"name":"Unsaved Flow"',
    );
  });

  it("flushes pending draft persistence when the panel unmounts before the debounce fires", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    const { unmount } = render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-unmount" />,
    );

    expect(
      await screen.findByText(
        "No orchestration templates yet. Start a new one and save it here.",
      ),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Template name"), {
      target: { value: "Unsaved Flow" },
    });

    const stateKey = "termal-orchestrator-panel-state:orchestrator-unmount";
    expect(window.localStorage.getItem(stateKey)).toBeNull();

    unmount();

    expect(window.localStorage.getItem(stateKey)).toContain(
      '"name":"Unsaved Flow"',
    );
  });

  it("allows cyclic transition drafts", async () => {
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
      screen.getByRole("button", { name: "Create template" }),
    ).toBeEnabled();
    expect(
      screen.queryByText(/cannot create cycles?/i),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText(/two different sessions/i),
    ).not.toBeInTheDocument();
  });

  it("allows self-loop transition drafts", async () => {
    fetchTemplatesMock.mockResolvedValue({ templates: [] });
    window.localStorage.setItem(
      "termal-orchestrator-panel-state:orchestrator-self-loop",
      JSON.stringify({
        draft: {
          name: "Self Loop Flow",
          description: "",
          projectId: null,
          sessions: [makeSession({ id: "loop", name: "Loop Session" })],
          transitions: [
            makeTransition({
              id: "loop-to-loop",
              fromSessionId: "loop",
              toSessionId: "loop",
            }),
          ],
        },
        selectedNodeId: "loop",
        selectedTemplateId: null,
      }),
    );

    const { container } = render(
      <OrchestratorTemplatesPanel persistenceKey="orchestrator-self-loop" />,
    );

    expect(
      await screen.findByDisplayValue("Self Loop Flow"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Create template" }),
    ).toBeEnabled();
    expect(
      screen.queryByText(/cannot create cycles?/i),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText(/two different sessions/i),
    ).not.toBeInTheDocument();
    const edgePath = container.querySelector(
      "svg.orchestrator-board-edges path.orchestrator-board-edge",
    );
    expect(edgePath).not.toBeNull();
    expect(edgePath?.getAttribute("d")).toContain(" C ");
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
        createdAt: "2026-03-30 09:00:00",
        completedAt: null,
      },
      state: makeStateResponse({
        revision: 2,
        projects: [makeProject()],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      }),
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
    expect(onStateUpdated).toHaveBeenCalledWith(
      makeStateResponse({
        revision: 2,
        projects: [makeProject()],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      }),
    );
    expect(
      screen.getByText("Orchestration started: orchestrator-1"),
    ).toBeInTheDocument();
  });

  it("starts the selected template for a remote project", async () => {
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
        id: "orchestrator-remote",
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
        createdAt: "2026-03-30 09:00:00",
        completedAt: null,
      },
      state: makeStateResponse({
        revision: 2,
        projects: [makeProject({ remoteId: "ssh-lab" })],
        orchestrators: [],
        workspaces: [],
        sessions: [],
      }),
    });

    render(
      <OrchestratorTemplatesPanel
        initialTemplateId="template-run"
        projects={[makeProject({ remoteId: "ssh-lab" })]}
      />,
    );

    expect(await screen.findByDisplayValue("Run Flow")).toBeInTheDocument();
    const runButton = screen
      .getAllByRole("button", { name: /run/i })
      .find((candidate) =>
        /Run on Project A/.test(candidate.getAttribute("title") ?? ""),
      );
    if (!runButton) {
      throw new Error("Run button not found");
    }
    expect(runButton).toBeEnabled();

    fireEvent.click(runButton);

    await waitFor(() => {
      expect(createOrchestratorInstanceMock).toHaveBeenCalledWith(
        "template-run",
        "project-a",
      );
    });
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

  it("shows the transition title without rendering an editable transition id field", async () => {
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

    fireEvent.click(await screen.findByTitle("transition-1: Dev -> Reviewer"));

    expect(
      screen.getByRole("heading", { level: 3, name: "transition-1" }),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("Transition id")).not.toBeInTheDocument();
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
    inputMode: "queue",
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

function makeRuntimeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: "runtime-session",
    name: "Runtime Session",
    emoji: "C",
    agent: "Codex",
    workdir: "/repo",
    projectId: "project-a",
    model: "gpt-5.4",
    modelOptions: [],
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}
