import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchOrchestratorTemplates } from "../api";
import type { OrchestratorTemplate } from "../types";
import { OrchestratorTemplateLibraryPanel } from "./OrchestratorTemplateLibraryPanel";

vi.mock("../api", () => ({
  fetchOrchestratorTemplates: vi.fn(),
}));

const fetchTemplatesMock = vi.mocked(fetchOrchestratorTemplates);

describe("OrchestratorTemplateLibraryPanel", () => {
  beforeEach(() => {
    fetchTemplatesMock.mockReset();
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

    fireEvent.click(screen.getByRole("button", { name: "Create template canvas" }));

    await waitFor(() => {
      expect(onNewCanvas).toHaveBeenCalledTimes(1);
    });
  });
});

function makeTemplate(overrides: Partial<OrchestratorTemplate> = {}): OrchestratorTemplate {
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
