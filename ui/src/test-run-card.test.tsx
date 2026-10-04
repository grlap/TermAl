// Test-run card rendering, transcript updates and navigation. No launcher,
// disk evidence, backend process, or production DOM geometry is exercised.
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Message, Session, TestRunCardMessage, TestRunCardUpdatedEvent } from "./types";
import { MessageCard } from "./message-cards";
import { TestRunsProvider } from "./test-runs-context";
import { makeTestRun, makeTestRunCard } from "./test-runs-fixtures";
import { TestRunsPanel } from "./panels/TestRunsPanel";
import { applyDeltaToSessions } from "./live-updates";
import { reconcileSessions } from "./session-reconcile";
import { buildSessionSearchMatches } from "./session-find";
import { messageChangeMarker } from "./app-utils";
import { isSameRevisionReplayableSessionDelta } from "./app-live-state-delta-events";
import { buildVirtualizedMessageLayout, estimateConversationMessageHeight, resolveEstimatedConversationMessageHeight } from "./panels/conversation-virtualization";

afterEach(cleanup);
const card = (overrides: Partial<TestRunCardMessage["run"]> = {}): TestRunCardMessage => ({
  id: "card-one", type: "testRun", author: "system", timestamp: "2026-09-26T10:00:00Z", schemaVersion: 1,
  run: makeTestRunCard(overrides),
});
const session = (message: Message): Session => ({ id: "owner", agent: "Codex", name: "Owner", emoji: "x",
  workdir: "/repo", model: "default", status: "idle", preview: "run", messages: [message], messageCount: 1, messagesLoaded: true });
const event = (message: TestRunCardMessage): TestRunCardUpdatedEvent => ({ type: "testRunCardUpdated",
  revision: 2, sessionId: "owner", messageId: message.id, messageIndex: 0, messageCount: 1,
  preview: "run", run: message.run });

describe("test-run cards", () => {
  it.each(["noPid", "heartbeatStale"] as const)("shows pending uncertainty without recovery advice for %s", unknownReason => {
    render(<MessageCard message={card({ state: "unknown", unknownReason })}
      onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
    expect(screen.getByText(/executor not published/)).toBeInTheDocument();
    expect(screen.queryByText(/wait remains pending/)).not.toBeInTheDocument();
    expect(screen.queryByText(/recover command/)).not.toBeInTheDocument();
  });
  it.each(["running", "passed", "failed", "unknown"] as const)("renders %s from the transcript rather than the live index", state => {
    const open = vi.fn();
    const message = card({ state });
    const view = render(<TestRunsProvider snapshotReady runs={[makeTestRun({ state: "passed" })]} open={open}>
      <MessageCard message={message} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />
    </TestRunsProvider>);
    expect(screen.getByText("System")).toBeInTheDocument();
    expect(view.container.querySelector(".command-status-chip")).toHaveTextContent(state);
    expect(view.container.querySelector('[data-activity="running"]') !== null).toBe(state === "running");
    if (state === "unknown") expect(view.container.querySelector(".chip-status-idle")).toBeNull();
    const details = screen.getByRole("button", { name: `Details for test run ${message.run.runId}` });
    expect(details).toHaveClass("command-icon-button");
    fireEvent.click(details);
    expect(open).toHaveBeenCalledWith(null, { runId: message.run.runId, runDir: message.run.runDir });
  });
  it("keeps failed excerpts as text, marks truncation and omitted stages", () => {
    render(<MessageCard message={card({ state: "failed", stagesOmitted: 3, commandTruncated: true,
      preset: "focused", command: ["node", "test.mjs"], error: "error", errorTruncated: true,
      failure: { phase: "stage", name: "focused", excerpt: "<script>alert(1)</script>", truncated: true },
    })} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);
    expect(screen.getByText(/node scripts\/test-launcher.mjs focused -- node test.mjs/)).toBeInTheDocument();
    expect(screen.getByText("<script>alert(1)</script>")).toBeInTheDocument();
    expect(screen.getByText(/3 stages omitted/)).toBeInTheDocument();
    expect(screen.getByText(/Excerpt truncated/)).toBeInTheDocument();
  });
  it("opens missing run evidence without querying or launching it", () => {
    const run = makeTestRunCard();
    const view = (ready: boolean) => <TestRunsProvider runs={[]} snapshotReady={ready} open={vi.fn()}>
      <TestRunsPanel projects={[]} sessions={[]} initialRun={run} />
    </TestRunsProvider>;
    const rendered = render(view(false));
    expect(screen.getByRole("heading", { name: "Waiting for the test-run index…" })).toBeInTheDocument();
    expect(screen.queryByText("Run no longer indexed")).not.toBeInTheDocument();
    rendered.rerender(view(true));
    expect(screen.getByRole("heading", { name: "Run not currently indexed" })).toBeInTheDocument();
    expect(screen.getByText(run.runDir)).toBeInTheDocument();
    expect(screen.getByText(/Discovery may still be in progress/)).toBeInTheDocument();
    expect(screen.getByText(/node scripts\/test-launcher.mjs summary/)).toHaveTextContent("summary RUN_DIRECTORY");
  });
  it.each([String.raw`C:\repo\$cash\$(calc)\a'"` + "`name", "/repo/$HOME/$(echo bad)/a'\"`name"])("keeps the literal evidence path out of the shell template: %s", runDir => {
    render(<TestRunsProvider snapshotReady runs={[]} open={vi.fn()}><TestRunsPanel projects={[]} sessions={[]} initialRun={{ runId: "run", runDir }} /></TestRunsProvider>);
    expect(screen.getByText(runDir).textContent).toBe(runDir);
    expect(screen.getByText(/node scripts\/test-launcher.mjs summary/).textContent).toBe("node scripts/test-launcher.mjs summary RUN_DIRECTORY");
  });
  it.each(["responsible process is gone", "node scripts/test-launcher.mjs full", "exit 9", "interrupted"])("finds visible card text: %s", query => {
    const message = card({ state: "unknown", unknownReason: "processGone", interrupted: true, exitCode: 9 });
    expect(buildSessionSearchMatches(session(message), query)).toHaveLength(1);
  });
  it("replaces the run snapshot while retaining message identity, timestamp and position", () => {
    const before = card();
    const next = card({ state: "failed", error: "failed stage" });
    const result = applyDeltaToSessions([session(before)], event(next));
    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") throw new Error("expected applied");
    expect(result.sessions[0].messages).toEqual([{ ...before, run: next.run }]);
    expect(applyDeltaToSessions(result.sessions, event(next)).kind).toBe("appliedNoOp");
    expect(isSameRevisionReplayableSessionDelta(event(next))).toBe(true);
    expect(messageChangeMarker(before)).not.toBe(messageChangeMarker(next));
    expect(buildSessionSearchMatches(result.sessions[0], "failed stage")).toHaveLength(1);
    const equal = reconcileSessions(result.sessions, structuredClone(result.sessions));
    expect(equal[0].messages[0]).toBe(result.sessions[0].messages[0]);
  });
  it("requests resync for a mismatched or missing retained target", () => {
    expect(applyDeltaToSessions([], event(card())).kind).toBe("needsResync");
    expect(applyDeltaToSessions([session({ id: "card-one", type: "text", author: "assistant", timestamp: "now", text: "x" })], event(card())).kind).toBe("needsResync");
    const missing = { ...session(card()), messages: [], messagesLoaded: false };
    expect(applyDeltaToSessions([missing], event(card())).kind).toBe("appliedNeedsResync");
  });
  it("keeps estimates and layout finite for cards and future unknown message types", () => {
    const unknown = { id: "future", type: "future", author: "system" } as unknown as Message;
    const cache = new WeakMap();
    const heights = [card(), unknown].map(message => resolveEstimatedConversationMessageHeight(cache, message,
      { expandedPromptOpen: false, viewportWidth: 600 }));
    expect(heights.every(height => Number.isFinite(height) && height > 0)).toBe(true);
    expect(estimateConversationMessageHeight(unknown)).toBe(heights[1]);
    expect(buildVirtualizedMessageLayout(heights).tops.every(Number.isFinite)).toBe(true);
  });
});
