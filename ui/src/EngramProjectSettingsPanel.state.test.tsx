// The panel's state wording for a declared project turned off in TermAl, and
// the reason it gives while "Save & enable" is unavailable. API replies are
// mocked and stepped explicitly; the live Engram CLI is not exercised.
import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  describeProjectEngramState,
  EngramProjectSettingsPanel,
} from "./EngramProjectSettingsPanel";
import {
  updateProjectEngramSettings,
  verifyProjectEngramSettings,
  type StateResponse,
} from "./api";
import {
  getAcceptancePolicy,
  type AcceptancePolicySnapshot,
} from "./acceptance-settings-api";
import type { EngramProjectVerification, Project } from "./types";

vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  updateProjectEngramSettings: vi.fn(),
  verifyProjectEngramSettings: vi.fn(),
}));
vi.mock("./acceptance-settings-api", () => ({
  getAcceptancePolicy: vi.fn(),
  changeAcceptancePolicy: vi.fn(),
  saveEvaluatorDefaults: vi.fn(),
}));

const turnedOffProject: Project = {
  id: "one",
  name: "One",
  rootPath: "/one",
  remoteId: "local",
  engramDeclared: true,
  engramOperatorDisabled: true,
  engram: { enabled: false, turnGatedControl: false },
};

const enabledProject: Project = {
  ...turnedOffProject,
  engramOperatorDisabled: false,
  engram: { enabled: true, turnGatedControl: false },
};

const policy: AcceptancePolicySnapshot = {
  available: true,
  readerKey: "reader",
  policy: "a".repeat(32),
  acceptanceEvaluation: {
    modes: [],
    mechanicalBasis: "asserted",
    requireSourceFreshness: false,
  },
};

const successfulVerification: EngramProjectVerification = {
  verified: true,
  projectId: "engram-one",
  ready: true,
  fullAudit: "not_run",
  hostPathStatus: "matched",
  elapsedMs: 12,
  binaryPath: "/bin/engram",
  home: "/one/.engram",
  database: "/one/.engram/store.db",
  requiredAssurance: "asserted",
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function renderPanel(project: Project) {
  render(
    <EngramProjectSettingsPanel
      project={project}
      idPrefix="test"
      onVerified={vi.fn()}
      onSaved={vi.fn()}
      onDefaultsSaved={vi.fn()}
    />,
  );
  // The embedded acceptance settings read the store policy only for an
  // enabled project; wait for whichever settled text applies.
  await screen.findByText(
    project.engram?.enabled
      ? "Off — completion is self-asserted."
      : "Enable Engram to read its store policy.",
  );
}

const saveButton = () => screen.getByRole("button", { name: "Save & enable" });

beforeEach(() => {
  sessionStorage.clear();
  vi.resetAllMocks();
  vi.mocked(getAcceptancePolicy).mockResolvedValue(policy);
  vi.mocked(verifyProjectEngramSettings).mockResolvedValue(
    successfulVerification,
  );
  vi.mocked(updateProjectEngramSettings).mockResolvedValue({
    projects: [enabledProject],
  } as StateResponse);
});

describe("describeProjectEngramState", () => {
  it("says a declared project turned off in TermAl is turned off, not vetoed", () => {
    const label = describeProjectEngramState(turnedOffProject);

    expect(label).toBe("Declared · turned off in TermAl");
    expect(label).not.toMatch(/veto/i);
  });
});

describe("EngramProjectSettingsPanel turned-off state", () => {
  it("names the state in plain words and the next step", async () => {
    await renderPanel(turnedOffProject);

    expect(
      screen.getByText("Declared · turned off in TermAl"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Engram is turned off for this project in TermAl. To turn it on: Verify, then Save & enable.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/veto/i)).toBeNull();
  });

  it("shows no turned-off guidance for an enabled project", async () => {
    await renderPanel(enabledProject);

    expect(
      screen.queryByText(/Engram is turned off for this project/),
    ).toBeNull();
  });
});

describe("EngramProjectSettingsPanel Save & enable availability", () => {
  it("says Save needs a successful Verify before one has run", async () => {
    await renderPanel(turnedOffProject);

    expect(saveButton()).toBeDisabled();
    expect(saveButton()).toHaveAccessibleDescription(
      "Save & enable needs a successful Verify first.",
    );
  });

  it("says Save is unavailable because the last Verify failed", async () => {
    vi.mocked(verifyProjectEngramSettings).mockResolvedValue({
      ...successfulVerification,
      verified: false,
      errors: ["cannot enable Engram: doctor reported an unhealthy store"],
    });
    await renderPanel(turnedOffProject);

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Verify" }));
    });

    expect(saveButton()).toBeDisabled();
    expect(saveButton()).toHaveAccessibleDescription(
      "Save & enable is unavailable: the last Verify failed. Fix the errors above and Verify again.",
    );
  });

  it("says Save waits while another action finishes", async () => {
    const pending = deferred<EngramProjectVerification>();
    vi.mocked(verifyProjectEngramSettings).mockReturnValue(pending.promise);
    await renderPanel(turnedOffProject);

    fireEvent.click(screen.getByRole("button", { name: "Verify" }));

    expect(saveButton()).toBeDisabled();
    expect(saveButton()).toHaveAccessibleDescription(
      "Save & enable is unavailable while another action finishes.",
    );

    await act(async () => {
      pending.resolve(successfulVerification);
    });
  });

  it("gives no reason once a successful Verify makes Save available", async () => {
    await renderPanel(turnedOffProject);

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Verify" }));
    });

    expect(saveButton()).toBeEnabled();
    expect(saveButton()).not.toHaveAttribute("aria-describedby");
    expect(screen.queryByText(/Save & enable needs/)).toBeNull();
    expect(screen.queryByText(/Save & enable is unavailable/)).toBeNull();
  });
});
