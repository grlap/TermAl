import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchOrchestratorTemplates,
  pauseOrchestratorInstance,
  resumeOrchestratorInstance,
  stopOrchestratorInstance,
} from "../api";
import { ORCHESTRATOR_TEMPLATES_CHANGED_EVENT } from "../orchestrator-templates-events";
import type { OrchestratorInstance, OrchestratorTemplate } from "../types";
import { OrchestratorTemplateLibraryPanel } from "./OrchestratorTemplateLibraryPanel";

vi.mock("../api", () => ({
  fetchOrchestratorTemplates: vi.fn(),
  pauseOrchestratorInstance: vi.fn(),
  resumeOrchestratorInstance: vi.fn(),
  stopOrchestratorInstance: vi.fn(),
}));

const fetchTemplatesMock = vi.mocked(fetchOrchestratorTemplates);
const pauseOrchestratorInstanceMock = vi.mocked(pauseOrchestratorInstance);
const resumeOrchestratorInstanceMock = vi.mocked(resumeOrchestratorInstance);
const stopOrchestratorInstanceMock = vi.mocked(stopOrchestratorInstance);

describe("OrchestratorTemplateLibraryPanel", () => {
  beforeEach(() => {
    fetchTemplatesMock.mockReset();
    pauseOrchestratorInstanceMock.mockReset();
    resumeOrchestratorInstanceMock.mockReset();
    stopOrchestratorInstanceMock.mockReset();
  });

  it("loads templates and opens an existing canvas", async () => {
    const onNewCanvas = vi.fn();
    const onOpenCanvas = vi.fn();
    fetchTemplatesMock.mockResolvedValue({
      templates: [makeTemplate()],
    });

    render(
      <OrchestratorTemplateLibraryPanel
        onNewCanvas={onNewCanvas}
        onOpenCanvas={onOpenCanvas}
      />,
    );

    expect(await screen.findByText("Delivery Flow")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Edit canvas" }));

    expect(onOpenCanvas).toHaveBeenCalledWith("template-1");
    expect(onNewCanvas).not.toHaveBeenCalled();
  });

  it("offers a blank canvas when the library is empty", async () => {
    const onNewCanvas = vi.fn();

    fetchTemplatesMock.mockResolvedValue({ templates: [] });

    render(
      <OrchestratorTemplateLibraryPanel
        onNewCanvas={onNewCanvas}
        onOpenCanvas={() => {}}
      />,
    );

    await screen.findByText(
      "No orchestration templates yet. Start with a blank canvas and save your first flow.",
    );

    fireEvent.click(
      screen.getByRole("button", { name: "Create template canvas" }),
    );

    await waitFor(() => {
      expect(onNewCanvas).toHaveBeenCalledTimes(1);
    });
  });

  it("shows a fetch error when loading templates fails", async () => {
    fetchTemplatesMock.mockRejectedValue(
      new Error("Could not load templates."),
    );

    render(
      <OrchestratorTemplateLibraryPanel
        onNewCanvas={() => {}}
        onOpenCanvas={() => {}}
      />,
    );

    expect(
      await screen.findByText("Could not load templates."),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Loading orchestration templates..."),
    ).not.toBeInTheDocument();
  });

  it("reloads templates when the library changed event fires", async () => {
    fetchTemplatesMock
      .mockResolvedValueOnce({
        templates: [makeTemplate({ id: "template-1", name: "Original Flow" })],
      })
      .mockResolvedValueOnce({
        templates: [makeTemplate({ id: "template-2", name: "Updated Flow" })],
      });

    render(
      <OrchestratorTemplateLibraryPanel
        onNewCanvas={() => {}}
        onOpenCanvas={() => {}}
      />,
    );

    expect(await screen.findByText("Original Flow")).toBeInTheDocument();

    await act(async () => {
      window.dispatchEvent(
        new CustomEvent(ORCHESTRATOR_TEMPLATES_CHANGED_EVENT),
      );
    });

    expect(await screen.findByText("Updated Flow")).toBeInTheDocument();
    expect(fetchTemplatesMock).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("Original Flow")).not.toBeInTheDocument();
  });

  it("runs lifecycle actions and forwards the returned state", async () => {
    const onStateUpdated = vi.fn();
    fetchTemplatesMock.mockResolvedValue({ templates: [makeTemplate()] });
    pauseOrchestratorInstanceMock.mockResolvedValue(makeStateResponse(2));
    resumeOrchestratorInstanceMock.mockResolvedValue(makeStateResponse(3));
    stopOrchestratorInstanceMock.mockResolvedValue(makeStateResponse(4));

    render(
      <OrchestratorTemplateLibraryPanel
        orchestrators={[
          makeOrchestrator({
            id: "runtime-running",
            status: "running",
            templateSnapshot: makeTemplate({
              id: "runtime-template-1",
              name: "Runtime Running",
            }),
          }),
          makeOrchestrator({
            id: "runtime-paused",
            status: "paused",
            templateSnapshot: makeTemplate({
              id: "runtime-template-2",
              name: "Runtime Paused",
            }),
          }),
        ]}
        onNewCanvas={() => {}}
        onOpenCanvas={() => {}}
        onStateUpdated={onStateUpdated}
      />,
    );

    const runningCard = (await screen.findByRole("heading", { name: "Runtime Running" })).closest("article");
    const pausedCard = screen.getByRole("heading", { name: "Runtime Paused" }).closest("article");
    if (!runningCard || !pausedCard) {
      throw new Error("Runtime cards not found");
    }

    fireEvent.click(within(runningCard).getByRole("button", { name: "Pause" }));
    await waitFor(() => {
      expect(pauseOrchestratorInstanceMock).toHaveBeenCalledWith("runtime-running");
      expect(onStateUpdated).toHaveBeenNthCalledWith(1, makeStateResponse(2));
    });

    fireEvent.click(within(pausedCard).getByRole("button", { name: "Resume" }));
    await waitFor(() => {
      expect(resumeOrchestratorInstanceMock).toHaveBeenCalledWith("runtime-paused");
      expect(onStateUpdated).toHaveBeenNthCalledWith(2, makeStateResponse(3));
    });

    fireEvent.click(within(runningCard).getByRole("button", { name: "Stop" }));
    await waitFor(() => {
      expect(stopOrchestratorInstanceMock).toHaveBeenCalledWith("runtime-running");
      expect(onStateUpdated).toHaveBeenNthCalledWith(3, makeStateResponse(4));
    });
  });
});

function makeTemplate(
  overrides: Partial<OrchestratorTemplate> = {},
): OrchestratorTemplate {
  return {
    id: "template-1",
    name: "Delivery Flow",
    description: "Implement and review the work.",
    createdAt: "2026-03-26 10:00:00",
    updatedAt: "2026-03-26 10:15:00",
    sessions: [
      {
        id: "builder",
        name: "Builder",
        agent: "Codex",
        model: null,
        instructions: "Implement the change.",
        autoApprove: true,
        inputMode: "queue",
        position: { x: 220, y: 420 },
      },
    ],
    transitions: [],
    ...overrides,
  };
}

function makeOrchestrator(
  overrides: Partial<OrchestratorInstance> = {},
): OrchestratorInstance {
  return {
    id: "orchestrator-1",
    templateId: "template-1",
    projectId: "project-1",
    templateSnapshot: makeTemplate(),
    status: "running",
    sessionInstances: [
      {
        templateSessionId: "builder",
        sessionId: "session-1",
        lastCompletionRevision: null,
        lastDeliveredCompletionRevision: null,
      },
    ],
    pendingTransitions: [],
    createdAt: "2026-03-26 10:20:00",
    completedAt: null,
    errorMessage: null,
    ...overrides,
  };
}

function makeStateResponse(revision: number) {
  return {
    revision,
    projects: [],
    orchestrators: [],
    sessions: [],
  };
}