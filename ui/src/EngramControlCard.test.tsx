import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { MessageCard } from "./message-cards";
import { messageChangeMarker } from "./app-utils";
import type { EngramControlMessage } from "./types";

function renderEngramCard(message: EngramControlMessage) {
  return render(
    <MessageCard
      message={message}
      onApprovalDecision={vi.fn()}
      onUserInputSubmit={vi.fn()}
    />,
  );
}

describe("EngramControlCard", () => {
  const causalMessage: EngramControlMessage = {
    id: "causal-card", type: "engramControl", author: "assistant", timestamp: "10:06",
    schemaVersion: 1, stage: "dispatch", assurance: "turn_gated", decision: "degraded",
    dispatch: "withheld", refusalCode: "control_circuit_open", latencyMs: { total: 2 }, failMode: "degraded",
    causalFailure: {
      operation: "turn_checkpoint", failureClass: "transport", originalCode: "control_unavailable",
      message: "EOF; response frame missing [redacted]", boundary: "TermAl host → Engram control",
      attemptId: "checkpoint-attempt-1", remoteApplication: "unknown",
      continuationReason: "Admission held after this unsettled checkpoint; no new Evaluate was sent.",
    },
  };

  it("shows the original operation and error separately from the downstream hold", () => {
    renderEngramCard(causalMessage);
    expect(screen.getByText(/Operation: turn_checkpoint/)).toHaveTextContent("Failure: transport");
    expect(screen.getByText(/Original code: control_unavailable/)).toBeInTheDocument();
    expect(screen.getByText(/EOF; response frame missing/)).toBeInTheDocument();
    expect(screen.getByText(/Boundary: TermAl host/)).toHaveTextContent("Remote application: unknown");
    expect(screen.getByText(/Attempt: checkpoint-attempt-1/)).toBeInTheDocument();
    expect(screen.getByText(/no new Evaluate was sent/)).toBeInTheDocument();
    expect(screen.queryByText(/Original failure details unavailable/)).not.toBeInTheDocument();
  });

  it("does not present an explicit refusal as proof of no prior application", () => {
    renderEngramCard({ ...causalMessage, causalFailure: {
      ...causalMessage.causalFailure!, failureClass: "producer_refusal", remoteApplication: "refused",
    } });
    expect(screen.getByText(/Remote application:/)).toHaveTextContent("request refused (prior application not determined)");
  });

  it("scopes never-started knowledge to this request and names an operator hold", () => {
    renderEngramCard({ ...causalMessage, causalFailure: {
      ...causalMessage.causalFailure!, remoteApplication: "not_started",
      continuationReason: "Operator paused this continuation; automatic retry is not admitted.",
    } });
    expect(screen.getByText(/Remote application:/)).toHaveTextContent("this request did not start (prior application not determined)");
    expect(screen.getByText(/Operator paused this continuation/)).toBeInTheDocument();
  });

  it.each(["control_request_expired", "control_store_busy"])("scopes %s non-application to this invocation", (code) => {
    renderEngramCard({ ...causalMessage, causalFailure: {
      ...causalMessage.causalFailure!, failureClass: "remote", originalCode: code, remoteApplication: "not_applied",
    } });
    expect(screen.getByText(/Remote application:/)).toHaveTextContent(
      "this invocation committed no changes (prior application not determined)",
    );
    expect(screen.queryByText(/this request did not start/)).not.toBeInTheDocument();
  });

  it("names a withheld begin's code and boundary when its cause is not published", () => {
    renderEngramCard({
      ...causalMessage,
      refusalCode: "begin_preparation_unacknowledged",
      causalFailure: undefined,
    });
    expect(screen.getByText(/Failure: begin_preparation_unacknowledged/)).toHaveTextContent(
      "Boundary: prepared-Begin durable acknowledgement",
    );
    expect(screen.queryByText(/Original failure details unavailable/)).not.toBeInTheDocument();
  });

  it("tells a pending prepared begin's automatic wait apart from a withheld one", () => {
    renderEngramCard({
      ...causalMessage,
      refusalCode: "begin_preparation_pending",
      causalFailure: undefined,
    });
    expect(screen.getByText(/Failure: begin_preparation_pending/)).toHaveTextContent(
      "Boundary: prepared-Begin durable acknowledgement (pending; the same Begin is kept for its retry)",
    );
    expect(screen.queryByText(/Original failure details unavailable/)).not.toBeInTheDocument();
  });

  it("never reads an inherited member as a code's boundary", () => {
    renderEngramCard({
      ...causalMessage,
      refusalCode: "constructor",
      causalFailure: undefined,
    });
    expect(screen.queryByText(/Boundary:/)).not.toBeInTheDocument();
    expect(screen.getByText(/Original failure details unavailable/)).toBeInTheDocument();
  });

  it("splits the latency into its measured phases", () => {
    renderEngramCard({
      ...causalMessage,
      latencyMs: { total: 20014, evaluate: 2883, admissionFence: 120, preparationFence: 17100 },
    });
    expect(
      screen.getByText(
        /Latency: 20014 ms \(evaluate 2883 ms, admission fence 120 ms, preparation fence 17100 ms\)/,
      ),
    ).toBeInTheDocument();
  });

  it("reports missing legacy cause and missing correlation honestly", () => {
    const { unmount } = renderEngramCard({ ...causalMessage, causalFailure: undefined });
    expect(screen.getByText(/Original failure details unavailable/)).toBeInTheDocument();
    unmount();
    renderEngramCard({ ...causalMessage, causalFailure: {
      ...causalMessage.causalFailure!, attemptId: undefined, originalCode: undefined,
    } });
    expect(screen.getByText(/Attempt: unavailable/)).toBeInTheDocument();
    expect(screen.getByText(/Original code: unavailable/)).toBeInTheDocument();
  });

  it("names the control process a failure was seen on, or says it is unavailable", () => {
    const { unmount } = renderEngramCard({ ...causalMessage, causalFailure: {
      ...causalMessage.causalFailure!,
      controlProcess: { identity: "pid 4242", exitState: "exited: exit code: 0" },
    } });
    expect(screen.getByText(/Control process:/)).toHaveTextContent(
      "Control process: pid 4242 · Exit state: exited: exit code: 0",
    );
    unmount();
    renderEngramCard(causalMessage);
    expect(screen.getByText(/Control process:/)).toHaveTextContent(
      "Control process: unavailable · Exit state: unavailable",
    );
  });

  it("updates causal content even when ordinary card fields are unchanged", () => {
    const changed: EngramControlMessage = { ...causalMessage, causalFailure: {
      ...causalMessage.causalFailure!, continuationReason: "Automatic retry scheduled pending durable acknowledgement.",
    } };
    expect(messageChangeMarker(changed)).not.toBe(messageChangeMarker(causalMessage));
    const { rerender, container } = renderEngramCard(causalMessage);
    rerender(<MessageCard message={changed} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
    expect(screen.getByText(/Automatic retry scheduled/)).toBeInTheDocument();
    expect(container.textContent).not.toContain("routing-secret");
    expect(container.textContent).not.toContain("delivery-secret");
  });

  it("renders the Engram decision separately from the host dispatch outcome", () => {
    renderEngramCard({
      id: "engram-defer-1",
      type: "engramControl",
      author: "assistant",
      timestamp: "10:00",
      schemaVersion: 1,
      stage: "dispatch",
      assurance: "advisory",
      decision: "defer",
      dispatch: "sent_without_grant",
      deferCode: "lease_busy",
      latencyMs: { evaluate: 17, total: 17 },
      failMode: "shadow",
    });

    expect(
      screen.getByRole("heading", { name: "Engram would defer this turn" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/Deferral: lease_busy/)).toHaveTextContent(
      "Dispatch: sent without grant",
    );
    expect(screen.queryByText(/Reason:/)).not.toBeInTheDocument();
  });

  it("makes a queued host outcome explicit", () => {
    renderEngramCard({
      id: "engram-queued-1",
      type: "engramControl",
      author: "assistant",
      timestamp: "10:01",
      schemaVersion: 1,
      stage: "restart",
      assurance: "advisory",
      decision: "degraded",
      dispatch: "queued",
      refusalCode: "rebind_failed",
      latencyMs: { total: 250 },
      failMode: "degraded",
      nextIntent: "wait",
    });

    expect(
      screen.getByRole("heading", { name: "Turn queued by Engram control" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/Dispatch: queued/)).toHaveTextContent(
      "Reason: rebind_failed",
    );
  });

  it("makes an armed issued-grant repair explicit", () => {
    renderEngramCard({
      id: "engram-repair-1",
      type: "engramControl",
      author: "assistant",
      timestamp: "10:02",
      schemaVersion: 1,
      stage: "dispatch",
      assurance: "advisory",
      decision: "refuse",
      dispatch: "sent_without_grant",
      refusalCode: "lifecycle_hold",
      latencyMs: { evaluate: 8, begin: 4, total: 12 },
      failMode: "shadow",
      repairArmed: true,
    });

    expect(screen.getByText(/Reason: lifecycle_hold/)).toHaveTextContent(
      "Repair: armed",
    );
  });

  it("names the source root a bound turn was measured in", () => {
    renderEngramCard({
      id: "engram-checkpoint-named",
      type: "engramControl",
      author: "assistant",
      timestamp: "10:03",
      schemaVersion: 1,
      stage: "checkpoint",
      assurance: "turn_gated",
      decision: "grant",
      dispatch: "sent_on_grant",
      grantId: "grant-1",
      latencyMs: { checkpoint: 3, total: 3 },
      failMode: "enforced",
      nextIntent: "wait",
      sourceRoot: { root: "C:\\repo\\.worktrees\\wt", workRef: "w-abc" },
    });

    expect(screen.getByText(/Grant: grant-1/)).toHaveTextContent(
      "Source basis: C:\\repo\\.worktrees\\wt (named for w-abc)",
    );
  });

  it("says when a bound turn was measured in the workdir because no worktree is named", async () => {
    renderEngramCard({
      id: "engram-checkpoint-workdir",
      type: "engramControl",
      author: "assistant",
      timestamp: "10:04",
      schemaVersion: 1,
      stage: "checkpoint",
      assurance: "turn_gated",
      decision: "grant",
      dispatch: "sent_on_grant",
      latencyMs: { checkpoint: 3, total: 3 },
      failMode: "enforced",
      sourceRoot: {},
    });

    // A routine checkpoint starts collapsed; its detail is one action away.
    await userEvent.click(screen.getByRole("button", { name: "Show Engram details" }));
    expect(screen.getByText(/Dispatch: sent on grant/)).toHaveTextContent(
      "Source basis: session workdir (no worktree named)",
    );
  });

  it("says nothing about a source basis on a card that carries none", async () => {
    renderEngramCard({
      id: "engram-dispatch-plain",
      type: "engramControl",
      author: "assistant",
      timestamp: "10:05",
      schemaVersion: 1,
      stage: "dispatch",
      assurance: "turn_gated",
      decision: "grant",
      dispatch: "sent_on_grant",
      latencyMs: { total: 1 },
      failMode: "enforced",
    });

    await userEvent.click(screen.getByRole("button", { name: "Show Engram details" }));
    expect(screen.getByText(/Dispatch: sent on grant/)).toBeInTheDocument();
    expect(screen.queryByText(/Source basis/)).not.toBeInTheDocument();
  });
});

describe("EngramControlCard default collapse", () => {
  const routineDispatch: EngramControlMessage = {
    id: "routine-dispatch", type: "engramControl", author: "assistant", timestamp: "11:00",
    schemaVersion: 1, stage: "dispatch", assurance: "turn_gated", decision: "grant",
    dispatch: "sent_on_grant", grantId: "grant-routine", latencyMs: { total: 4 }, failMode: "enforced",
  };
  const routineCheckpoint: EngramControlMessage = {
    ...routineDispatch, id: "routine-checkpoint", stage: "checkpoint", nextIntent: "continue",
    latencyMs: { checkpoint: 2, total: 2 },
  };

  function toggle() {
    return screen.queryByRole("button", { name: /Engram details/ });
  }

  it.each([
    ["dispatch", routineDispatch, "Turn admitted"],
    ["checkpoint", routineCheckpoint, "Turn checkpointed"],
  ])("collapses a routine %s card to one row carrying its title", async (_stage, message, title) => {
    const { container } = renderEngramCard(message);
    // One row: the meta row holds author, stage, title, toggle and time, and
    // nothing else is rendered.
    const card = container.querySelector(".engram-control-card")!;
    expect(card).toHaveClass("engram-control-compact");
    expect(Array.from(card.children).map((child) => child.className)).toEqual(["message-meta"]);
    const row = card.querySelector(".message-meta")!;
    expect(row).toHaveTextContent("Agent");
    expect(row).toHaveTextContent(`Engram · ${message.stage}`);
    expect(row).toHaveTextContent(title);
    expect(row).toHaveTextContent(message.timestamp);
    expect(screen.queryByRole("heading")).not.toBeInTheDocument();
    expect(card.querySelector(".card-label")).toBeNull();
    expect(screen.queryByText(/Grant: grant-routine/)).not.toBeInTheDocument();
    const button = screen.getByRole("button", { name: "Show Engram details" });
    expect(row).toContainElement(button);
    expect(button).toHaveAttribute("aria-expanded", "false");

    await userEvent.click(button);
    const expanded = screen.getByRole("button", { name: "Hide Engram details" });
    expect(expanded).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".engram-control-compact")).toBeNull();
    expect(screen.getByRole("heading", { name: title })).toBeInTheDocument();
    expect(screen.getByText(`Engram · ${message.stage}`)).toHaveClass("card-label");
    const region = document.getElementById(expanded.getAttribute("aria-controls")!);
    expect(region).toHaveTextContent("Grant: grant-routine");
    expect(region).toHaveTextContent("Dispatch: sent on grant");

    await userEvent.click(screen.getByRole("button", { name: "Hide Engram details" }));
    expect(screen.queryByText(/Grant: grant-routine/)).not.toBeInTheDocument();
    expect(screen.queryByRole("heading")).not.toBeInTheDocument();
    expect(container.querySelector(".engram-control-compact .message-meta")).toHaveTextContent(title);
  });

  it("expands and collapses from the keyboard", async () => {
    const user = userEvent.setup();
    renderEngramCard(routineDispatch);
    await user.tab();
    expect(screen.getByRole("button", { name: "Show Engram details" })).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(screen.getByText(/Grant: grant-routine/)).toBeInTheDocument();
    await user.keyboard(" ");
    expect(screen.queryByText(/Grant: grant-routine/)).not.toBeInTheDocument();
  });

  it("collapses a routine checkpoint that exits", () => {
    renderEngramCard({ ...routineCheckpoint, nextIntent: "exit" });
    expect(toggle()).toHaveAttribute("aria-expanded", "false");
  });

  const causalFailure: NonNullable<EngramControlMessage["causalFailure"]> = {
    operation: "turn_checkpoint", failureClass: "transport", message: "checkpoint failed",
    boundary: "TermAl host → Engram control", remoteApplication: "unknown",
    continuationReason: "Admission held after this unsettled checkpoint.",
  };
  const directive = { directiveId: "d-1", kind: "operator_action", audience: "operator", satisfaction: "satisfied" };

  // Each signal alone, on an otherwise routine grant, keeps the card expanded
  // with no collapse affordance, and its detail visible.
  it.each<[string, Partial<EngramControlMessage>, RegExp]>([
    ["an error cause", { causalFailure }, /checkpoint failed/],
    ["a degraded decision", { decision: "degraded", refusalCode: "control_circuit_open" }, /Reason: control_circuit_open/],
    ["a degraded fail mode", { failMode: "degraded" }, /Dispatch: sent on grant/],
    ["a shadow fail mode", { failMode: "shadow" }, /Dispatch: sent on grant/],
    ["a refusal", { decision: "refuse", refusalCode: "lifecycle_hold" }, /Reason: lifecycle_hold/],
    ["a refusal code on a grant", { refusalCode: "lifecycle_hold" }, /Reason: lifecycle_hold/],
    ["a deferral", { decision: "defer", deferCode: "lease_busy" }, /Deferral: lease_busy/],
    ["a deferral code on a grant", { deferCode: "lease_busy" }, /Deferral: lease_busy/],
    ["a queued dispatch", { dispatch: "queued" }, /Dispatch: queued/],
    ["a withheld dispatch", { dispatch: "withheld" }, /Dispatch: withheld/],
    ["a dispatch sent without a grant", { dispatch: "sent_without_grant" }, /Dispatch: sent without grant/],
    ["a waiting next intent", { stage: "checkpoint", nextIntent: "wait" }, /Next intent: wait/],
    ["an armed repair", { repairArmed: true }, /Repair: armed/],
    ["a required action", { directives: [directive] }, /operator_action · operator · satisfied/],
    ["a restart", { stage: "restart" }, /Dispatch: sent on grant/],
    ["an unknown schema", { schemaVersion: 2 }, /Dispatch: sent on grant/],
    ["an advisory assurance", { assurance: "advisory" }, /Dispatch: sent on grant/],
    ["an unsupported assurance", { assurance: "verified" }, /Dispatch: sent on grant/],
    ["an unknown stage", { stage: "future" as unknown as EngramControlMessage["stage"] }, /Dispatch: sent on grant/],
    ["an unknown dispatch", { dispatch: "future" as unknown as EngramControlMessage["dispatch"] }, /Dispatch: queued/],
    ["an unknown fail mode", { failMode: "future" as unknown as EngramControlMessage["failMode"] }, /Dispatch: sent on grant/],
    ["an unknown next intent", { nextIntent: "future" as unknown as EngramControlMessage["nextIntent"] }, /Next intent: future/],
  ])("keeps a card with %s expanded", (_signal, change, detail) => {
    const { container } = renderEngramCard({ ...routineDispatch, ...change });
    expect(toggle()).not.toBeInTheDocument();
    expect(screen.getByText(detail)).toBeInTheDocument();
    // The full, prominent card: its own label and heading, never the row.
    expect(container.querySelector(".engram-control-compact")).toBeNull();
    expect(container.querySelector(".engram-control-card .card-label")).not.toBeNull();
    expect(screen.getByRole("heading")).toBeInTheDocument();
  });

  it("keeps an actionable update visible when the same card stops being routine", () => {
    const { rerender } = renderEngramCard(routineCheckpoint);
    expect(screen.queryByText(/Grant: grant-routine/)).not.toBeInTheDocument();
    for (const change of [
      { causalFailure },
      { nextIntent: "wait" as const },
      { dispatch: "withheld" as const },
      { failMode: "shadow" as const },
      { repairArmed: true },
      { directives: [directive] },
      { deferCode: "lease_busy" },
    ]) {
      const changed: EngramControlMessage = { ...routineCheckpoint, ...change };
      // Only classification fields change; the card keeps its id.
      expect(messageChangeMarker(changed)).not.toBe(messageChangeMarker(routineCheckpoint));
      rerender(<MessageCard message={changed} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
      expect(toggle()).not.toBeInTheDocument();
      expect(screen.getByText(/Grant: grant-routine/)).toBeInTheDocument();
      rerender(<MessageCard message={routineCheckpoint} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
      expect(screen.queryByText(/Grant: grant-routine/)).not.toBeInTheDocument();
    }
    rerender(<MessageCard message={{ ...routineCheckpoint, causalFailure }} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
    expect(screen.getByText(/checkpoint failed/)).toBeInTheDocument();
  });

  it("keeps a manual expansion across ordinary rerenders and resets it for another card", async () => {
    const { rerender } = renderEngramCard(routineDispatch);
    await userEvent.click(screen.getByRole("button", { name: "Show Engram details" }));
    rerender(<MessageCard message={{ ...routineDispatch, latencyMs: { total: 9 } }} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
    expect(screen.getByText(/Latency: 9 ms/)).toBeInTheDocument();
    expect(toggle()).toHaveAttribute("aria-expanded", "true");
    rerender(<MessageCard message={{ ...routineDispatch, id: "another-routine-dispatch" }} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
    expect(toggle()).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText(/Grant: grant-routine/)).not.toBeInTheDocument();
  });

  it("marks every classification field in the change marker", () => {
    const base = messageChangeMarker(routineDispatch);
    const changes: Partial<EngramControlMessage>[] = [
      { schemaVersion: 2 },
      { assurance: "advisory" },
      { dispatch: "withheld" },
      { failMode: "shadow" },
      { deferCode: "lease_busy" },
      { repairArmed: true },
      { repairArmed: false },
      { nextIntent: "wait" },
      { directives: [directive] },
      { directives: [] },
    ];
    for (const change of changes) {
      expect(messageChangeMarker({ ...routineDispatch, ...change })).not.toBe(base);
    }
    expect(messageChangeMarker({ ...routineDispatch })).toBe(base);
  });
});

describe("EngramControlCard default collapse with search and markers", () => {
  const routine: EngramControlMessage = {
    id: "routine-search", type: "engramControl", author: "assistant", timestamp: "11:10",
    schemaVersion: 1, stage: "dispatch", assurance: "turn_gated", decision: "grant",
    dispatch: "sent_on_grant", grantId: "grant-searchable", latencyMs: { total: 4 }, failMode: "enforced",
  };

  it("opens a routine card while a transcript search is active", () => {
    const { rerender } = render(
      <MessageCard message={routine} searchQuery="grant-searchable" onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />,
    );
    expect(screen.getByText(/Grant: grant-searchable/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Engram details/ })).not.toBeInTheDocument();
    rerender(<MessageCard message={routine} searchQuery="" onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
    expect(screen.queryByText(/Grant: grant-searchable/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Show Engram details" })).toBeInTheDocument();
  });

  it("never lets a literal value collide with an absent field in the marker", () => {
    const base = messageChangeMarker(routine);
    for (const change of [
      { deferCode: "none" },
      { refusalCode: "none" },
      { nextIntent: "none" as unknown as EngramControlMessage["nextIntent"] },
      { repairArmed: "none" as unknown as boolean },
    ]) {
      expect(messageChangeMarker({ ...routine, ...change })).not.toBe(base);
    }
  });
});
