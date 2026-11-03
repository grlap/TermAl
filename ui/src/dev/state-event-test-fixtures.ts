// Owns synthetic full-state wire and targeted transcript fixtures for previews.
// Does not adopt state, schedule events, or contact a backend.
import type { StateResponse } from "../api";
import { resolveAppPreferences } from "../session-model-utils";
import type { Session, StateSessionSummary } from "../types";
import type { WorkspaceState } from "../workspace-types";

export const FIXTURE_SERVER_INSTANCE = "isolated-ui-instance";
export const FIXTURE_WORKSPACE_ID = "isolated-ui-workspace";

export function createStateEventWorkload(sessionCount = 8327) {
  if (!Number.isSafeInteger(sessionCount) || sessionCount < 2) {
    throw new Error("The fixture needs at least two sessions.");
  }
  const sessions = new Map<string, Session>();
  const summaries: StateSessionSummary[] = [];
  for (let index = 0; index < sessionCount; index += 1) {
    const summary: StateSessionSummary = {
      id: `synthetic-session-${index}`,
      name: index < 2 ? `Conversation ${index === 0 ? "A" : "B"}` : `Synthetic session ${index}`,
      emoji: "S", agent: "Codex", model: "default", workdir: "/synthetic",
      projectId: "synthetic-project", status: "idle", preview: "Synthetic fixture",
      messageCount: index < 2 ? 800 : 0, queuePaused: false,
      sessionMutationStamp: index + 1, bodySeq: 0, bodySeqEpoch: FIXTURE_SERVER_INSTANCE,
    };
    summaries.push(summary);
    sessions.set(summary.id, {
      ...summary, messagesLoaded: true, messageStartIndex: 0,
      hasOlderHistory: false, hasNewerHistory: false,
      messages: Array.from({ length: summary.messageCount }, (_, messageIndex) => ({
        id: `${summary.id}-message-${messageIndex}`, type: "text" as const,
        author: messageIndex % 2 ? "assistant" as const : "you" as const,
        timestamp: "12:00",
        text: `Message ${messageIndex}\n\n` +
          "A synthetic conversation paragraph with enough text to wrap across several lines. ".repeat(8),
      })),
    });
  }
  const tabs = [0, 1].map(index => ({ id: `conversation-${index === 0 ? "a" : "b"}`,
    kind: "session" as const, sessionId: `synthetic-session-${index}` }));
  const workspace: WorkspaceState = {
    root: { type: "pane", paneId: "fixture-pane" },
    panes: [{ id: "fixture-pane", tabs, activeTabId: tabs[0].id,
      activeSessionId: tabs[0].sessionId, viewMode: "session", lastSessionViewMode: "session",
      sourcePath: null }],
    activePaneId: "fixture-pane", lastContentPaneId: "fixture-pane", lastViewerPaneId: null,
  };

  function stateAt(revision: number): StateResponse {
    return {
      revision, serverInstanceId: FIXTURE_SERVER_INSTANCE,
      codex: {}, agentReadiness: [], preferences: resolveAppPreferences(),
      projects: [{ id: "synthetic-project", name: "Synthetic project", rootPath: "/synthetic" }],
      orchestrators: [], workspaces: [], delegations: [], delegationWaits: [],
      testRuns: [], testRunWaits: [],
      // Unrelated background metadata changes; visible transcripts stay fixed.
      sessions: summaries.map((summary, index) => index === sessionCount - 1
        ? { ...summary, preview: `Fixture revision ${revision}`,
            sessionMutationStamp: sessionCount + revision }
        : summary),
    };
  }

  function preparePackets(count = 24) {
    if (!Number.isSafeInteger(count) || count < 1 || count > 64) {
      throw new Error("Prepare between 1 and 64 finite fixture events.");
    }
    // Build wire outside the measured interaction, not in an event timer.
    return Array.from({ length: count }, (_, index) => {
      const state = stateAt(index + 2);
      const wire = JSON.stringify({ ...state, stateDiffBase: true });
      return { state, wire, bytes: new TextEncoder().encode(wire).length };
    });
  }

  return { sessions, workspace, stateAt, preparePackets };
}

export type StateEventWorkload = ReturnType<typeof createStateEventWorkload>;
export type StateEventPacket = ReturnType<StateEventWorkload["preparePackets"]>[number];
