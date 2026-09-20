// Integrated connection/defaults draft and busy-ownership regressions.
// Does not exercise the live Engram CLI; API replies are explicitly stepped.
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import { EngramProjectSettingsPanel } from "./EngramProjectSettingsPanel";
import { updateProjectEngramSettings, verifyProjectEngramSettings, type StateResponse } from "./api";
import { changeAcceptancePolicy, getAcceptancePolicy, saveEvaluatorDefaults, type AcceptancePolicySnapshot } from "./acceptance-settings-api";
import type { Project } from "./types";

vi.mock("./api", async importOriginal => ({ ...await importOriginal<typeof import("./api")>(),
  updateProjectEngramSettings: vi.fn(), verifyProjectEngramSettings: vi.fn() }));
vi.mock("./acceptance-settings-api", () => ({ getAcceptancePolicy: vi.fn(), changeAcceptancePolicy: vi.fn(), saveEvaluatorDefaults: vi.fn() }));

const project: Project = { id: "one", name: "One", rootPath: "/one", remoteId: "local", engramDeclared: true,
  engram: { enabled: true, turnGatedControl: false } };
const other: Project = { ...project, id: "two", name: "Two", rootPath: "/two" };
const policy: AcceptancePolicySnapshot = { available: true, readerKey: "reader", policy: "a".repeat(32),
  acceptanceEvaluation: { modes: [], mechanicalBasis: "asserted", requireSourceFreshness: false } };
const state = (project: Project) => ({ projects: [project] }) as StateResponse;

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}

function setup() {
  const busy = vi.fn();
  function Harness() {
    const [current, setCurrent] = useState(project);
    return <>
      <button onClick={() => setCurrent(other)}>Switch project</button>
      <button onClick={() => setCurrent(value => ({ ...value, engram: { ...value.engram!, turnGatedControl: true } }))}>Persist turn gating</button>
      <EngramProjectSettingsPanel project={current} idPrefix="test" onBusyChange={busy}
        onVerified={vi.fn()} onSaved={result => setCurrent(result.projects[0])}
        onDefaultsSaved={result => setCurrent(result.projects[0])} />
    </>;
  }
  render(<Harness />);
  return busy;
}

beforeEach(() => {
  sessionStorage.clear();
  vi.resetAllMocks();
  vi.mocked(getAcceptancePolicy).mockResolvedValue(policy);
  vi.mocked(verifyProjectEngramSettings).mockResolvedValue({ verified: true, projectId: "engram-one", ready: true,
    fullAudit: "not_run", hostPathStatus: "matched", elapsedMs: 12,
    binaryPath: "/bin/engram", home: "/one/.engram", database: "/one/.engram/store.db", requiredAssurance: "asserted" });
  vi.mocked(saveEvaluatorDefaults).mockImplementation(async (_, defaults) => state({ ...project, engram: { ...project.engram!, acceptanceEvaluation: defaults } }));
  vi.mocked(updateProjectEngramSettings).mockResolvedValue(state(project));
});

it("keeps turn-gating edits and verification when evaluator defaults are edited and saved", async () => {
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  fireEvent.click(screen.getByLabelText("Turn-gated control"));
  fireEvent.click(screen.getByRole("button", { name: "Verify" }));
  await screen.findByText("engram-one");
  fireEvent.change(screen.getByLabelText("Evaluator agent"), { target: { value: "Claude" } });
  expect(screen.getByRole("button", { name: "Save & enable" })).toBeEnabled();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save evaluator defaults" })); });
  expect(screen.getByLabelText("Turn-gated control")).toBeChecked();
  expect(screen.getByLabelText("Evaluator agent")).toHaveValue("Claude");
  expect(screen.getByRole("button", { name: "Save & enable" })).toBeEnabled();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save & enable" })); });
  expect(updateProjectEngramSettings).toHaveBeenCalledWith("one", { enabled: true, turnGatedControl: true });
  expect(verifyProjectEngramSettings).toHaveBeenCalledTimes(1);
});

it("keeps an unsaved evaluator draft through connection updates, resetting it only for another project", async () => {
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  fireEvent.change(screen.getByLabelText("Evaluator agent"), { target: { value: "Claude" } });
  fireEvent.click(screen.getByRole("button", { name: "Persist turn gating" }));
  expect(screen.getByLabelText("Turn-gated control")).toBeChecked();
  expect(screen.getByLabelText("Evaluator agent")).toHaveValue("Claude");
  fireEvent.click(screen.getByRole("button", { name: "Switch project" }));
  await screen.findByText("Off — completion is self-asserted.");
  expect(screen.getByLabelText("Evaluator agent")).toHaveValue("");
  expect(screen.getByLabelText("Turn-gated control")).not.toBeChecked();
});

it.each(["defaults", "policy"] as const)("releases %s busy ownership on a project switch without letting the old reply unlock a new save", async kind => {
  type Reply = StateResponse & AcceptancePolicySnapshot;
  const first = deferred<Reply>();
  const second = deferred<Reply>();
  const api = kind === "defaults" ? vi.mocked(saveEvaluatorDefaults) : vi.mocked(changeAcceptancePolicy);
  api.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
  const busy = setup();
  async function save() {
    if (kind === "defaults") fireEvent.click(screen.getByRole("button", { name: "Save evaluator defaults" }));
    else {
      fireEvent.click(screen.getByRole("button", { name: "Change store policy…" }));
      fireEvent.change(screen.getByLabelText("Policy change reason"), { target: { value: "Confirmed decision" } });
      fireEvent.click(screen.getByLabelText("I confirm this store-wide policy change"));
      fireEvent.click(screen.getByRole("button", { name: "Confirm policy change" }));
    }
    await waitFor(() => expect(screen.getByRole("button", { name: "Verify" })).toBeDisabled());
  }
  await screen.findByText("Off — completion is self-asserted.");
  await save();
  fireEvent.click(screen.getByRole("button", { name: "Switch project" }));
  await screen.findByText("Off — completion is self-asserted.");
  expect(screen.getByRole("button", { name: "Verify" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Disable Engram" })).toBeEnabled();
  expect(busy).toHaveBeenLastCalledWith(false);
  await save();
  await act(async () => first.resolve({ ...state(project), ...policy }));
  expect(screen.getByRole("button", { name: "Verify" })).toBeDisabled();
  expect(busy).toHaveBeenLastCalledWith(true);
  await act(async () => second.resolve({ ...state(other), ...policy }));
  expect(screen.getByRole("button", { name: "Verify" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Disable Engram" })).toBeEnabled();
  expect(busy).toHaveBeenLastCalledWith(false);
});
