import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import { AcceptanceEvaluationSettings } from "./AcceptanceEvaluationSettings";
import { ApiRequestError } from "./api-request";
import { changeAcceptancePolicy, getAcceptancePolicy, saveEvaluatorDefaults, type EvaluatorDefaults, type AcceptancePolicySnapshot } from "./acceptance-settings-api";
import type { StateResponse } from "./api";

vi.mock("./acceptance-settings-api", () => ({ getAcceptancePolicy: vi.fn(), changeAcceptancePolicy: vi.fn(), saveEvaluatorDefaults: vi.fn() }));
const policy = { available: true, readerKey: "reader", policy: "a".repeat(32), epoch: 1,
  acceptanceEvaluation: { modes: [] as ("same_session" | "independent_session")[], mechanicalBasis: "asserted" as const, requireSourceFreshness: false } };

function setup(enabled = true) {
  const saved = vi.fn();
  function Harness() {
    const [value, onChange] = useState<EvaluatorDefaults>({});
    return <AcceptanceEvaluationSettings projectId="project" enabled={enabled} value={value} onChange={onChange}
      busy={false} onBusyChange={vi.fn()} onSaved={saved} />;
  }
  const view = render(<Harness />);
  return { saved, unmount: view.unmount };
}

beforeEach(() => {
  sessionStorage.clear();
  vi.resetAllMocks();
  vi.mocked(getAcceptancePolicy).mockResolvedValue(policy);
  vi.mocked(changeAcceptancePolicy).mockResolvedValue(policy);
  vi.mocked(saveEvaluatorDefaults).mockResolvedValue({} as StateResponse);
});

it("reads policy without enabling and saves defaults through the dedicated endpoint", async () => {
  const { saved } = setup();
  await screen.findByText("Off — completion is self-asserted.");
  fireEvent.change(screen.getByLabelText("Evaluator agent"), { target: { value: "Claude" } });
  fireEvent.click(screen.getByRole("button", { name: "Save evaluator defaults" }));
  await waitFor(() => expect(saveEvaluatorDefaults).toHaveBeenCalledWith("project", { evaluatorAgent: "Claude" }));
  expect(saved).toHaveBeenCalled();
  expect(changeAcceptancePolicy).not.toHaveBeenCalled();
});

it("does not read a disabled store and does not report unavailable policy as off", async () => {
  setup(false);
  expect(screen.getByText("Enable Engram to read its store policy.")).toBeInTheDocument();
  expect(getAcceptancePolicy).not.toHaveBeenCalled();
});

it("shows unknown policy and disables editing for an older binary", async () => {
  vi.mocked(getAcceptancePolicy).mockResolvedValue({ available: false, readerKey: "reader", error: "unsupported command" });
  setup();
  await screen.findByText("Policy unknown — unsupported command");
  expect(screen.getByRole("button", { name: "Change store policy…" })).toBeDisabled();
});

async function confirmChange() {
  await screen.findByText("Off — completion is self-asserted.");
  fireEvent.click(screen.getByRole("button", { name: "Change store policy…" }));
  fireEvent.click(screen.getByLabelText("Allow Independent session"));
  fireEvent.change(screen.getByLabelText("Policy change reason"), { target: { value: "Require review" } });
  expect(screen.getByRole("button", { name: "Confirm policy change" })).toBeDisabled();
  fireEvent.click(screen.getByLabelText("I confirm this store-wide policy change"));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Confirm policy change" })); });
}

it("requires explicit confirmation and replays the exact change/key after a lost response", async () => {
  vi.mocked(changeAcceptancePolicy).mockRejectedValueOnce(new ApiRequestError("request-failed", "Outcome unknown", { status: 502 }));
  setup();
  await confirmChange();
  await screen.findByText("Outcome unknown");
  expect(screen.getByLabelText("Policy change reason")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Change store policy…" })).toBeDisabled();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry identical policy change" })); });
  await waitFor(() => expect(changeAcceptancePolicy).toHaveBeenCalledTimes(2));
  const calls = vi.mocked(changeAcceptancePolicy).mock.calls;
  expect(calls[0]).toEqual(calls[1]);
  expect(calls[0][1]).toMatchObject({ expectedPolicy: policy.policy, readerKey: "reader", modes: ["independent_session"], reason: "Require review" });
  expect(calls[0][1].idempotencyKey).toMatch(/^termal-policy-/);
});

it("refreshes policy on stale CAS rather than silently retrying against a new policy", async () => {
  vi.mocked(changeAcceptancePolicy).mockRejectedValueOnce(new ApiRequestError("request-failed", "Policy changed", { status: 409 }));
  setup();
  await confirmChange();
  await waitFor(() => expect(getAcceptancePolicy).toHaveBeenCalledTimes(2));
  expect(changeAcceptancePolicy).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("button", { name: "Retry identical policy change" })).not.toBeInTheDocument();
});

it("keeps an open draft tied to its original policy even after refresh", async () => {
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  fireEvent.click(screen.getByRole("button", { name: "Change store policy…" }));
  fireEvent.click(screen.getByLabelText("Allow Independent session"));
  fireEvent.change(screen.getByLabelText("Policy change reason"), { target: { value: "Draft against original" } });
  fireEvent.click(screen.getByLabelText("I confirm this store-wide policy change"));
  vi.mocked(getAcceptancePolicy).mockResolvedValue({ ...policy, policy: "b".repeat(32), readerKey: "new-reader",
    acceptanceEvaluation: { ...policy.acceptanceEvaluation, requireSourceFreshness: true } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Refresh policy" })); });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Confirm policy change" })); });
  expect(vi.mocked(changeAcceptancePolicy).mock.calls[0][1]).toMatchObject({ expectedPolicy: policy.policy, readerKey: "reader" });
});

it("recovers the identical uncertain attempt after unmount and does not require a fresh policy read", async () => {
  vi.mocked(changeAcceptancePolicy).mockRejectedValueOnce(new ApiRequestError("request-failed", "Unknown", { status: 502 }));
  const view = setup();
  await confirmChange();
  const original = vi.mocked(changeAcceptancePolicy).mock.calls[0][1];
  view.unmount();
  vi.mocked(getAcceptancePolicy).mockResolvedValue({ available: false, readerKey: "other", error: "offline" });
  setup();
  await screen.findByText("Policy unknown — offline");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry identical policy change" })); });
  expect(vi.mocked(changeAcceptancePolicy).mock.calls[1][1]).toEqual(original);
  expect(sessionStorage.length).toBe(0);
});

it("keeps a definitive first parser refusal correctable", async () => {
  vi.mocked(changeAcceptancePolicy).mockRejectedValueOnce(new ApiRequestError("request-failed", "Command was not sent", { status: 400 }));
  setup();
  await confirmChange();
  expect(screen.getByLabelText("Policy change reason")).toBeEnabled();
  expect(screen.getByRole("button", { name: "Cancel policy change" })).toBeEnabled();
  expect(sessionStorage.length).toBe(0);
});

it("refuses to send when recovery storage cannot retain the request", async () => {
  const write = vi.spyOn(sessionStorage, "setItem").mockImplementation(() => { throw new Error("quota"); });
  setup();
  await confirmChange();
  expect(changeAcceptancePolicy).not.toHaveBeenCalled();
  expect(screen.getByRole("alert")).toHaveTextContent("No policy change was sent");
  write.mockRestore();
});

it.each(["{broken", JSON.stringify({ modes: [] })])("fails closed on corrupt recovery data %s and explains the tab remedy", async raw => {
  sessionStorage.setItem("termal.acceptance-policy.pending.v1:project", raw);
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  expect(screen.getByRole("button", { name: "Change store policy…" })).toBeDisabled();
  expect(screen.getByRole("alert")).toHaveTextContent("closing this browser tab clears its saved attempt");
  expect(changeAcceptancePolicy).not.toHaveBeenCalled();
  expect(sessionStorage.getItem("termal.acceptance-policy.pending.v1:project")).toBe(raw);
});

it("blocks sends when recovery storage cannot be read", async () => {
  const read = vi.spyOn(sessionStorage, "getItem").mockImplementation(() => { throw new Error("blocked"); });
  try {
    setup();
    await screen.findByText("Off — completion is self-asserted.");
    expect(screen.getByRole("button", { name: "Change store policy…" })).toBeDisabled();
    expect(screen.getByRole("alert")).toHaveTextContent("Restore browser session storage");
    expect(changeAcceptancePolicy).not.toHaveBeenCalled();
  } finally { read.mockRestore(); }
});

it("ignores a pre-write refresh that resolves after the newer policy write", async () => {
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  fireEvent.click(screen.getByRole("button", { name: "Change store policy…" }));
  fireEvent.click(screen.getByLabelText("Allow Independent session"));
  fireEvent.change(screen.getByLabelText("Policy change reason"), { target: { value: "New policy" } });
  fireEvent.click(screen.getByLabelText("I confirm this store-wide policy change"));
  let resolveRead!: (snapshot: AcceptancePolicySnapshot) => void;
  vi.mocked(getAcceptancePolicy).mockImplementationOnce(() => new Promise(resolve => { resolveRead = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "Refresh policy" }));
  await waitFor(() => expect(getAcceptancePolicy).toHaveBeenCalledTimes(2));
  const newer = { ...policy, policy: "b".repeat(32), epoch: 2,
    acceptanceEvaluation: { ...policy.acceptanceEvaluation, modes: ["independent_session" as const] } };
  vi.mocked(changeAcceptancePolicy).mockResolvedValueOnce(newer);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Confirm policy change" })); });
  await screen.findByText(/Required · Independent session/);
  await act(async () => resolveRead(policy)); // Deliberately ignores AbortSignal.
  expect(screen.queryByText("Off — completion is self-asserted.")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Change store policy…" }));
  fireEvent.change(screen.getByLabelText("Policy change reason"), { target: { value: "Next decision" } });
  fireEvent.click(screen.getByLabelText("I confirm this store-wide policy change"));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Confirm policy change" })); });
  expect(vi.mocked(changeAcceptancePolicy).mock.calls[1][1].expectedPolicy).toBe(newer.policy);
});

it("retains the original uncertain payload across a reset 409 and remount", async () => {
  vi.mocked(changeAcceptancePolicy)
    .mockRejectedValueOnce(new ApiRequestError("request-failed", "Response lost", { status: 502 }))
    .mockRejectedValueOnce(new ApiRequestError("request-failed", "Engram project reset is in progress", { status: 409 }));
  const view = setup();
  await confirmChange();
  const original = vi.mocked(changeAcceptancePolicy).mock.calls[0][1];
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry identical policy change" })); });
  expect(screen.getByRole("alert")).toHaveTextContent("reset is in progress");
  expect(sessionStorage.length).toBe(1);
  view.unmount();
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry identical policy change" })); });
  expect(vi.mocked(changeAcceptancePolicy).mock.calls.map(call => call[1])).toEqual([original, original, original]);
  expect(sessionStorage.length).toBe(0);
});

it("reports an applied change with an unavailable post-write snapshot without retrying", async () => {
  vi.mocked(changeAcceptancePolicy).mockResolvedValueOnce({ available: false, readerKey: "reader", writeApplied: true,
    error: "Policy change applied to the selected store, but project settings changed" });
  setup();
  await confirmChange();
  expect(screen.getByRole("alert")).toHaveTextContent("Policy change applied");
  expect(screen.queryByRole("button", { name: "Retry identical policy change" })).not.toBeInTheDocument();
  expect(sessionStorage.length).toBe(0);
  expect(screen.getByRole("button", { name: "Change store policy…" })).toBeDisabled();
});

it("binds model defaults to a concrete agent and enforces the UTF-8 byte limit", async () => {
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  expect(screen.getByLabelText("Evaluator model override")).toBeDisabled();
  expect(screen.getByRole("option", { name: /Sub-agent — not produced/ })).toBeDisabled();
  fireEvent.change(screen.getByLabelText("Default evaluator mode"), { target: { value: "sub_agent" } });
  expect(screen.getByLabelText("Default evaluator mode")).toHaveValue("");
  fireEvent.change(screen.getByLabelText("Evaluator agent"), { target: { value: "Claude" } });
  fireEvent.change(screen.getByLabelText("Evaluator model override"), { target: { value: "界".repeat(43) } });
  fireEvent.click(screen.getByRole("button", { name: "Save evaluator defaults" }));
  expect(screen.getByRole("alert")).toHaveTextContent("128 UTF-8 bytes");
  expect(saveEvaluatorDefaults).not.toHaveBeenCalled();
  fireEvent.change(screen.getByLabelText("Evaluator agent"), { target: { value: "Codex" } });
  expect(screen.getByLabelText("Evaluator model override")).toHaveValue("");
  fireEvent.change(screen.getByLabelText("Evaluator agent"), { target: { value: "" } });
  expect(screen.getByLabelText("Evaluator model override")).toBeDisabled();
});

it("does not let a late response from a closed dialog erase a newer uncertain attempt", async () => {
  let resolveOriginal!: (snapshot: AcceptancePolicySnapshot) => void;
  vi.mocked(changeAcceptancePolicy).mockImplementationOnce(() => new Promise(resolve => { resolveOriginal = resolve; }));
  const originalView = setup();
  await confirmChange();
  originalView.unmount();
  const recoveryView = setup();
  await screen.findByText("Off — completion is self-asserted.");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry identical policy change" })); });
  expect(sessionStorage.length).toBe(0);
  vi.mocked(changeAcceptancePolicy).mockRejectedValueOnce(new ApiRequestError("request-failed", "New response lost", { status: 502 }));
  await confirmChange();
  const newerAttempt = vi.mocked(changeAcceptancePolicy).mock.calls[2][1];
  await act(async () => resolveOriginal(policy));
  expect(sessionStorage.length).toBe(1);
  recoveryView.unmount();
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry identical policy change" })); });
  expect(vi.mocked(changeAcceptancePolicy).mock.calls[3][1]).toEqual(newerAttempt);
});

it("clears a whitespace-only model override before saving defaults", async () => {
  setup();
  await screen.findByText("Off — completion is self-asserted.");
  fireEvent.change(screen.getByLabelText("Evaluator agent"), { target: { value: "Claude" } });
  fireEvent.change(screen.getByLabelText("Evaluator model override"), { target: { value: "   " } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save evaluator defaults" })); });
  expect(saveEvaluatorDefaults).toHaveBeenCalledWith("project", { evaluatorAgent: "Claude", evaluatorModel: undefined });
});

it("keeps a known applied result visible while failed recovery cleanup blocks new policy writes", async () => {
  const remove = vi.spyOn(sessionStorage, "removeItem").mockImplementation(() => { throw new Error("blocked"); });
  try {
    vi.mocked(changeAcceptancePolicy).mockResolvedValueOnce({ ...policy, writeApplied: true,
      acceptanceEvaluation: { ...policy.acceptanceEvaluation, modes: ["independent_session"] } });
    setup();
    await confirmChange();
    expect(screen.getByText(/Required · Independent session/)).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("Policy change applied, but recovery storage could not be cleared");
    expect(screen.queryByText(/The write may have succeeded/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry identical policy change" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Change store policy…" })).toBeDisabled();
    expect(changeAcceptancePolicy).toHaveBeenCalledTimes(1);
    expect(sessionStorage.length).toBe(1);
  } finally { remove.mockRestore(); }
});

it("explains how to resume an uncertain policy attempt after disabling Engram", async () => {
  vi.mocked(changeAcceptancePolicy).mockRejectedValueOnce(new ApiRequestError("request-failed", "Unknown", { status: 502 }));
  const view = setup();
  await confirmChange();
  view.unmount();
  setup(false);
  expect(screen.getByRole("button", { name: "Retry identical policy change" })).toBeDisabled();
  expect(screen.getByText(/Re-enable Engram for the original store/)).toBeInTheDocument();
  expect(sessionStorage.length).toBe(1);
  expect(changeAcceptancePolicy).toHaveBeenCalledTimes(1);
});

it("replaces a failed recovery GET with the confirmed policy from an identical retry", async () => {
  vi.mocked(changeAcceptancePolicy).mockRejectedValueOnce(new ApiRequestError("request-failed", "Unknown", { status: 502 }));
  const view = setup();
  await confirmChange();
  view.unmount();
  vi.mocked(getAcceptancePolicy).mockRejectedValueOnce(new Error("Read unavailable"));
  vi.mocked(changeAcceptancePolicy).mockResolvedValueOnce({ ...policy, writeApplied: true,
    acceptanceEvaluation: { ...policy.acceptanceEvaluation, modes: ["independent_session"] } });
  setup();
  await screen.findByText("Policy unknown — Read unavailable");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry identical policy change" })); });
  expect(screen.getByText(/Required · Independent session/)).toBeInTheDocument();
  expect(screen.queryByText(/Policy unknown/)).not.toBeInTheDocument();
  expect(sessionStorage.length).toBe(0);
});
