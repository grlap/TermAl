import { render, screen } from "@testing-library/react";
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

  it("says when a bound turn was measured in the workdir because no worktree is named", () => {
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

    expect(screen.getByText(/Dispatch: sent on grant/)).toHaveTextContent(
      "Source basis: session workdir (no worktree named)",
    );
  });

  it("says nothing about a source basis on a card that carries none", () => {
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

    expect(screen.queryByText(/Source basis/)).not.toBeInTheDocument();
  });
});
