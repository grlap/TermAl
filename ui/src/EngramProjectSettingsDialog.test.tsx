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
    executionObservability: false,
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
    fireEvent.click(screen.getByRole("button", { name: "Verify" }));
    await screen.findByText("engram-project-1");
    fireEvent.click(screen.getByRole("button", { name: "Change store policy…" }));
    fireEvent.click(screen.getByLabelText("Allow Independent session"));
    fireEvent.click(screen.getByRole("combobox", { name: "Evaluator agent" }));
    fireEvent.click(screen.getByRole("option", { name: "Claude" }));
    fireEvent.click(screen.getByRole("button", { name: "Save evaluator defaults" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Save evaluator defaults" })).toBeEnabled());
    expect(saveEvaluatorDefaults).toHaveBeenCalledWith("project-1", { evaluatorAgent: "Claude" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.queryByLabelText("Turn-gated control")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save & enable" })).toBeEnabled();
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
      executionObservability: false,
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
      executionObservability: false,
    });
    expect(callbacks.onSaved).toHaveBeenCalledTimes(1);
    expect(callbacks.onClose).toHaveBeenCalledTimes(1);
  });

  it("does not expose or resend persisted legacy turn gating", async () => {
    await renderDialog({ engram: { ...project.engram!, ...JSON.parse('{"turnGatedControl":true}') } });
    expect(screen.queryByLabelText("Turn-gated control")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Verify" }));
    await screen.findByText("engram-project-1");

    expect(mockVerifyProjectEngramSettings).toHaveBeenCalledWith(
      "project-1",
      { enabled: true, executionObservability: false },
    );

    fireEvent.click(screen.getByRole("button", { name: "Save & enable" }));
    await waitFor(() =>
      expect(mockUpdateProjectEngramSettings).toHaveBeenCalledWith(
        "project-1",
        { enabled: true, executionObservability: false },
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
      expect.objectContaining({ verified: true, detail: "Disabled in TermAl" }),
    );
  });

  it("disables a project that retains legacy turn-gating settings", async () => {
    mockUpdateProjectEngramSettings.mockResolvedValueOnce({
      projects: [
        {
          ...project,
          engram: {
            ...project.engram!,
            enabled: false,
            executionObservability: false,
          },
        },
      ],
    } as StateResponse);
    await renderDialog({
      engram: {
        ...project.engram!,
        executionObservability: true,
      },
    });
    expect(screen.queryByLabelText("Turn-gated control")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Disable Engram" }));

    await waitFor(() => expect(mockUpdateProjectEngramSettings).toHaveBeenCalledWith(
      "project-1", { enabled: false },
    ));
  });

  it("still shows observations off after Disable", async () => {
    mockUpdateProjectEngramSettings.mockResolvedValueOnce({
      projects: [{ ...project, engram: { ...project.engram!, enabled: false } }],
    } as StateResponse);
    function Harness() {
      const [current, setCurrent] = useState(project);
      return <EngramProjectSettingsDialog project={current} onClose={vi.fn()}
        onVerified={vi.fn()} onSaved={state => setCurrent(state.projects[0])} />;
    }
    render(<Harness />);
    await screen.findByText("Policy unknown — Policy unavailable");
    expect(screen.getByLabelText("Execution Observability")).not.toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "Disable Engram" }));

    await screen.findByText("Declared · ready to enable");
    expect(mockUpdateProjectEngramSettings).toHaveBeenCalledWith("project-1", { enabled: false });
    expect(screen.getByLabelText("Execution Observability")).not.toBeChecked();
  });

  it("resynchronizes observability from persisted project state", async () => {
    const props = {
      onClose: vi.fn(),
      onSaved: vi.fn(),
      onVerified: vi.fn(),
    };
    const view = render(
      <EngramProjectSettingsDialog project={project} {...props} />,
    );
    await screen.findByText("Policy unknown — Policy unavailable");
    expect(screen.queryByLabelText("Turn-gated control")).not.toBeInTheDocument();

    view.rerender(
      <EngramProjectSettingsDialog
        project={{
          ...project,
          engram: { ...project.engram!, executionObservability: true },
        }}
        {...props}
      />,
    );

    expect(screen.queryByLabelText("Turn-gated control")).not.toBeInTheDocument();
    await screen.findByText("Policy unknown — Policy unavailable");
  });
});


it.each([false, true])("defaults observability ON independently of an imported old flag (%s)", async oldFlag => {
  await renderDialog({ engram: { enabled: true, ...JSON.parse(JSON.stringify({ turnGatedControl: oldFlag })) } });
  expect(screen.getByLabelText("Execution Observability")).toBeChecked();
  expect(screen.queryByLabelText("Turn-gated control")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Verify" }));
  await screen.findByText("engram-project-1");
  expect(mockVerifyProjectEngramSettings).toHaveBeenCalledWith("project-1", {
    enabled: true, executionObservability: true,
  });
  expect(screen.queryByText("Required assurance")).not.toBeInTheDocument();
});

it.each([false, true])("saves the explicit observability switch from %s without losing connection verification", async initial => {
  await renderDialog({ engram: { ...project.engram!, executionObservability: initial } });
  fireEvent.click(screen.getByRole("button", { name: "Verify" }));
  await screen.findByText("engram-project-1");
  fireEvent.click(screen.getByLabelText("Execution Observability"));
  expect(screen.getByRole("button", { name: "Save & enable" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: "Save & enable" }));
  await waitFor(() => expect(mockUpdateProjectEngramSettings).toHaveBeenCalledWith("project-1", {
    enabled: true, executionObservability: !initial,
  }));
  expect(mockVerifyProjectEngramSettings).toHaveBeenCalledTimes(1);
});
