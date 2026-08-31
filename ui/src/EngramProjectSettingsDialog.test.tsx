import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  updateProjectEngramSettings,
  verifyProjectEngramSettings,
  type StateResponse,
} from "./api";
import { EngramProjectSettingsDialog } from "./EngramProjectSettingsDialog";
import type { EngramProjectVerification, Project } from "./types";

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
  engramGrantConfigured: true,
  engram: {
    enabled: true,
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
  healthy: true,
  grant: {
    configured: true,
    installed: true,
    subjectActorId: "termal",
    validFrom: "2026-01-01T00:00:00Z",
    validUntil: "2027-01-01T00:00:00Z",
    revokedAt: null,
    valid: true,
  },
};

function renderDialog(overrides: Partial<Project> = {}) {
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
  return { onClose, onSaved, onVerified };
}

beforeEach(() => {
  vi.clearAllMocks();
  mockUpdateProjectEngramSettings.mockResolvedValue({} as StateResponse);
  mockVerifyProjectEngramSettings.mockResolvedValue(successfulVerification);
});

describe("EngramProjectSettingsDialog", () => {
  it("keeps the saved grant write-only and requires Verify before Save", async () => {
    const callbacks = renderDialog();
    const grantInput = screen.getByLabelText("Work authority grant");
    const saveButton = screen.getByRole("button", { name: "Save & enable" });

    expect(grantInput).toHaveAttribute("type", "password");
    expect(grantInput).toHaveValue("");
    expect(saveButton).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Verify" }));

    await screen.findByText("engram-project-1");
    expect(mockVerifyProjectEngramSettings).toHaveBeenCalledTimes(1);
    const verificationPayload =
      mockVerifyProjectEngramSettings.mock.calls[0][1];
    expect(verificationPayload).not.toHaveProperty("workAuthorityGrant");
    expect(verificationPayload).not.toHaveProperty("binaryPath");
    expect(verificationPayload).not.toHaveProperty("home");
    expect(verificationPayload).not.toHaveProperty("deadlineMs");
    expect(saveButton).toBeEnabled();

    fireEvent.click(saveButton);

    await waitFor(() =>
      expect(mockUpdateProjectEngramSettings).toHaveBeenCalledTimes(1),
    );
    const savePayload = mockUpdateProjectEngramSettings.mock.calls[0][1];
    expect(savePayload).not.toHaveProperty("workAuthorityGrant");
    expect(callbacks.onSaved).toHaveBeenCalledTimes(1);
    expect(callbacks.onClose).toHaveBeenCalledTimes(1);
  });

  it("sends a newly entered grant to Verify and Save without exposing it in status", async () => {
    renderDialog();
    const secret = "grant-new-secret";
    const grantInput = screen.getByLabelText("Work authority grant");
    fireEvent.change(grantInput, { target: { value: secret } });

    fireEvent.click(screen.getByRole("button", { name: "Verify" }));
    await screen.findByText("engram-project-1");

    expect(mockVerifyProjectEngramSettings).toHaveBeenCalledWith(
      "project-1",
      expect.objectContaining({ workAuthorityGrant: secret }),
    );
    expect(screen.queryByText(secret)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Save & enable" }));
    await waitFor(() =>
      expect(mockUpdateProjectEngramSettings).toHaveBeenCalledWith(
        "project-1",
        expect.objectContaining({ workAuthorityGrant: secret }),
      ),
    );
  });

  it("keeps Save blocked when verification reports revoked authority", async () => {
    mockVerifyProjectEngramSettings.mockResolvedValue({
      ...successfulVerification,
      verified: false,
      grant: {
        ...successfulVerification.grant,
        revokedAt: "2026-08-30T22:26:46.8Z",
        valid: false,
      },
      errors: ["cannot enable Engram: work-authority grant is revoked"],
    });
    renderDialog();

    fireEvent.click(screen.getByRole("button", { name: "Verify" }));

    await screen.findByText("Verification failed");
    expect(screen.getAllByText(/grant is revoked/)).toHaveLength(2);
    expect(
      screen.getByRole("button", { name: "Save & enable" }),
    ).toBeDisabled();
  });

  it("allows disabling without Verify as a recovery action", async () => {
    const callbacks = renderDialog();
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
});
