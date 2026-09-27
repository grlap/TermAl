import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MessageCard } from "./message-cards";
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
