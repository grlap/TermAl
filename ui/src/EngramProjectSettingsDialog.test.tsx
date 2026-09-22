import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  updateProjectEngramSettings,
  verifyProjectEngramSettings,
  type StateResponse,
} from "./api";
import { EngramProjectSettingsDialog } from "./EngramProjectSettingsDialog";
import type { EngramProjectVerification, Project } from "./types";
import { getAcceptancePolicy, saveEvaluatorDefaults } from "./acceptance-settings-api";

vi.mock("./acceptance-settings-api", () => ({
  getAcceptancePolicy: vi.fn().mockResolvedValue({ available: false, readerKey: "test", error: "Policy unavailable" }),
  saveEvaluatorDefaults: vi.fn(), changeAcceptancePolicy: vi.fn(),
}));

vi.mock("./api", async (importOriginal) => {
  const original = await importOriginal<typeof import("./api")>();
  return {
    ...original,
    updateProjectEngramSettings: vi.fn(),
    verifyProjectEngramSettings: vi.fn(),
  };
});

const mockUpdateProjectEngramSettings = vi.mocked(updateProjectEngramSettings);
const mockVerifyProjectEngramSettings = vi.mocked(verifyProjectEngramSettings);

const project: Project = {
  id: "project-1",
  name: "TermAl",
  rootPath: "C:\\github\\Personal\\TermAl",
  remoteId: "local",
  engramDeclared: true,
  engram: {
    enabled: true,
    turnGatedControl: false,
    binaryPath: "engram",
    home: "C:\\Users\\greg\\.engram",
    deadlineMs: 2000,
  },
};

const successfulVerification: EngramProjectVerification = {
  verified: true,
  binaryPath: "engram",
  home: "C:\\Users\\greg\\.engram",
  projectId: "engram-project-1",
  database: "C:\\Users\\greg\\.engram\\engram.db",
  requiredAssurance: "turn_gated",
  ready: true,
  fullAudit: "not_run",
  hostPathStatus: "matched",
  elapsedMs: 12,
};

async function renderDialog(overrides: Partial<Project> = {}) {
  const onClose = vi.fn();
  const onSaved = vi.fn();
  const onVerified = vi.fn();
  render(
    <EngramProjectSettingsDialog
      project={{ ...project, ...overrides }}
      onClose={onClose}
      onSaved={onSaved}
      onVerified={onVerified}
    />,
  );
  await screen.findByText("Policy unknown — Policy unavailable");
  return { onClose, onSaved, onVerified };
}

beforeEach(() => {
  vi.clearAllMocks();
  mockUpdateProjectEngramSettings.mockResolvedValue({} as StateResponse);
  mockVerifyProjectEngramSettings.mockResolvedValue(successfulVerification);
});

describe("EngramProjectSettingsDialog", () => {
  it("saves defaults without closing the dialog or discarding connection and policy drafts", async () => {
    const onClose = vi.fn();
    vi.mocked(getAcceptancePolicy).mockResolvedValueOnce({
      available: true, readerKey: "reader", policy: "a".repeat(32),
      acceptanceEvaluation: { modes: [], mechanicalBasis: "asserted", requireSourceFreshness: false },
    });
    vi.mocked(saveEvaluatorDefaults).mockImplementationOnce(async (_, defaults) => ({
      projects: [{ ...project, engram: { ...project.engram!, acceptanceEvaluation: defaults } }],
    }) as StateResponse);
    function Harness() {
      const [current, setCurrent] = useState(project);
      const [open, setOpen] = useState(true);
      return open ? <EngramProjectSettingsDialog project={current} onVerified={vi.fn()}
        onSaved={state => setCurrent(state.projects[0])}
        onClose={() => { onClose(); setOpen(false); }} /> : null;
    }
    render(<Harness />);
    await screen.findByText("Off — completion is self-asserted.");
    fireEvent.click(screen.getByLabelText("Turn-gated control"));
    fireEvent.click(screen.getByRole("button", { name: "Change store policy…" }));
    fireEvent.click(screen.getByLabelText("Allow Independent session"));
    fireEvent.click(screen.getByRole("combobox", { name: "Evaluator agent" }));
    fireEvent.click(screen.getByRole("option", { name: "Claude" }));
    fireEvent.click(screen.getByRole("button", { name: "Save evaluator defaults" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Save evaluator defaults" })).toBeEnabled());
    expect(saveEvaluatorDefaults).toHaveBeenCalledWith("project-1", { evaluatorAgent: "Claude" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.getByLabelText("Turn-gated control")).toBeChecked();
    expect(screen.getByLabelText("Allow Independent session")).toBeChecked();
    expect(screen.queryByLabelText("Policy change reason")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Evaluator agent")).toHaveTextContent("Claude");
    expect(updateProjectEngramSettings).not.toHaveBeenCalled();
  });

  it("requires Verify before enabling the base tier", async () => {
    const callbacks = await renderDialog();
    const saveButton = screen.getByRole("button", { name: "Save & enable" });

    expect(saveButton).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Verify" }));

    await screen.findByText("engram-project-1");
    expect(mockVerifyProjectEngramSettings).toHaveBeenCalledTimes(1);
    const verificationPayload =
      mockVerifyProjectEngramSettings.mock.calls[0][1];
    expect(verificationPayload).toEqual({
      enabled: true,
      turnGatedControl: false,
    });
    expect(verificationPayload).not.toHaveProperty("binaryPath");
    expect(verificationPayload).not.toHaveProperty("home");
    expect(verificationPayload).not.toHaveProperty("deadlineMs");
    expect(saveButton).toBeEnabled();

    fireEvent.click(saveButton);

    await waitFor(() =>
      expect(mockUpdateProjectEngramSettings).toHaveBeenCalledTimes(1),
    );
    const savePayload = mockUpdateProjectEngramSettings.mock.calls[0][1];
    expect(savePayload).toEqual({
      enabled: true,
      turnGatedControl: false,
    });
    expect(callbacks.onSaved).toHaveBeenCalledTimes(1);
    expect(callbacks.onClose).toHaveBeenCalledTimes(1);
  });

  it("keeps premium turn-gated control as an explicit opt-in", async () => {
    await renderDialog();
    fireEvent.click(screen.getByLabelText("Turn-gated control"));

    fireEvent.click(screen.getByRole("button", { name: "Verify" }));
    await screen.findByText("engram-project-1");

    expect(mockVerifyProjectEngramSettings).toHaveBeenCalledWith(
      "project-1",
      expect.objectContaining({ enabled: true, turnGatedControl: true }),
    );

    fireEvent.click(screen.getByRole("button", { name: "Save & enable" }));
    await waitFor(() =>
      expect(mockUpdateProjectEngramSettings).toHaveBeenCalledWith(
        "project-1",
        expect.objectContaining({ enabled: true, turnGatedControl: true }),
      ),
    );
  });

  it("keeps Save blocked when project verification fails", async () => {
    mockVerifyProjectEngramSettings.mockResolvedValue({
      ...successfulVerification,
      verified: false,
      errors: ["cannot enable Engram: doctor reported an unhealthy store"],
    });
    await renderDialog();

    fireEvent.click(screen.getByRole("button", { name: "Verify" }));

    await screen.findByText("Verification failed");
    expect(screen.getAllByText(/unhealthy store/)).toHaveLength(2);
    expect(
      screen.getByRole("button", { name: "Save & enable" }),
    ).toBeDisabled();
  });

  it("allows disabling without Verify as a recovery action", async () => {
    const callbacks = await renderDialog();
    const disableButton = screen.getByRole("button", {
      name: "Disable Engram",
    });

    expect(disableButton).toBeEnabled();
    fireEvent.click(disableButton);

    await waitFor(() =>
      expect(mockUpdateProjectEngramSettings).toHaveBeenCalledWith(
        "project-1",
        expect.objectContaining({ enabled: false }),
      ),
    );
    expect(mockVerifyProjectEngramSettings).not.toHaveBeenCalled();
    expect(callbacks.onVerified).toHaveBeenCalledWith(
      "project-1",
      expect.objectContaining({ verified: true, detail: "Operator vetoed" }),
    );
  });

  it("resets the premium draft from the persisted disable response", async () => {
    mockUpdateProjectEngramSettings.mockResolvedValueOnce({
      projects: [
        {
          ...project,
          engram: {
            ...project.engram!,
            enabled: false,
            turnGatedControl: false,
          },
        },
      ],
    } as StateResponse);
    await renderDialog({
      engram: {
        ...project.engram!,
        turnGatedControl: true,
      },
    });
    const premiumToggle = screen.getByLabelText("Turn-gated control");
    expect(premiumToggle).toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "Disable Engram" }));

    await waitFor(() => expect(premiumToggle).not.toBeChecked());
  });

  it("resynchronizes the premium draft when persisted project state changes", async () => {
    const props = {
      onClose: vi.fn(),
      onSaved: vi.fn(),
      onVerified: vi.fn(),
    };
    const view = render(
      <EngramProjectSettingsDialog project={project} {...props} />,
    );
    await screen.findByText("Policy unknown — Policy unavailable");
    const premiumToggle = screen.getByLabelText("Turn-gated control");
    expect(premiumToggle).not.toBeChecked();

    view.rerender(
      <EngramProjectSettingsDialog
        project={{
          ...project,
          engram: { ...project.engram!, turnGatedControl: true },
        }}
        {...props}
      />,
    );

    expect(premiumToggle).toBeChecked();
    await screen.findByText("Policy unknown — Policy unavailable");
  });
});
