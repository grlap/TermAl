import { describe, expect, it, vi } from "vitest";

import { reconcileAdoptedSessionsWorkspace } from "./app-live-state-workspace-reconciliation";
import type { Session } from "./types";
import type { WorkspaceState, WorkspaceTab } from "./workspace";

function makeSession(id: string, parentDelegationId: string | null = null): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Codex",
    workdir: "/tmp",
    model: "test-model",
    status: "idle",
    preview: "",
    messages: [],
    ...(parentDelegationId ? { parentDelegationId } : {}),
  };
}

function makeSingleSessionWorkspace(sessionId: string): WorkspaceState {
  return makeWorkspaceWithTabs(
    [
      {
        id: `tab-${sessionId}`,
        kind: "session",
        sessionId,
      },
    ],
    `tab-${sessionId}`,
    sessionId,
  );
}

function makeWorkspaceWithTabs(
  tabs: WorkspaceTab[],
  activeTabId: string | null = tabs[0]?.id ?? null,
  activeSessionId: string | null = null,
): WorkspaceState {
  return {
    root: {
      type: "pane",
      paneId: "pane-1",
    },
    panes: [
      {
        id: "pane-1",
        tabs,
        activeTabId,
        activeSessionId,
        viewMode: activeSessionId ? "session" : "canvas",
        lastSessionViewMode: "session",
        sourcePath: null,
      },
    ],
    activePaneId: "pane-1",
  };
}

describe("reconcileAdoptedSessionsWorkspace", () => {
  it("prunes delegated child session tabs even when session array identity is unchanged", () => {
    const parentSession = makeSession("parent-session");
    const childSession = makeSession("child-session", "delegation-1");
    const workspace = makeSingleSessionWorkspace(childSession.id);
    const applyControlPanelLayout = vi.fn((nextWorkspace: WorkspaceState) => nextWorkspace);

    const result = reconcileAdoptedSessionsWorkspace({
      applyControlPanelLayout,
      canOpenPendingSession: false,
      current: workspace,
      mergedSessions: [parentSession, childSession],
      pendingPaneId: null,
      pruneDelegatedChildWorkspaceTabs: true,
      sessionsChanged: false,
    });

    expect(result).not.toBe(workspace);
    expect(applyControlPanelLayout).toHaveBeenCalledTimes(1);
    expect(result.panes[0].tabs).toEqual([
      expect.objectContaining({
        kind: "session",
        sessionId: parentSession.id,
      }),
    ]);
    expect(result.panes[0].activeSessionId).toBe(parentSession.id);
  });

  it("detects delegated child references in canvas cards and origin session fields", () => {
    const parentSession = makeSession("parent-session");
    const childSession = makeSession("child-session", "delegation-1");
    const workspace = makeWorkspaceWithTabs([
      {
        id: "canvas-tab",
        kind: "canvas",
        cards: [
          { sessionId: childSession.id, x: 0, y: 0 },
          { sessionId: parentSession.id, x: 10, y: 20 },
        ],
        originSessionId: null,
      },
      {
        id: "source-tab",
        kind: "source",
        path: "/tmp/example.ts",
        originSessionId: childSession.id,
      },
    ]);
    const applyControlPanelLayout = vi.fn((nextWorkspace: WorkspaceState) => nextWorkspace);

    const result = reconcileAdoptedSessionsWorkspace({
      applyControlPanelLayout,
      canOpenPendingSession: false,
      current: workspace,
      mergedSessions: [parentSession, childSession],
      pendingPaneId: null,
      pruneDelegatedChildWorkspaceTabs: true,
      sessionsChanged: false,
    });

    const [canvasTab, sourceTab] = result.panes[0].tabs;
    expect(applyControlPanelLayout).toHaveBeenCalledTimes(1);
    expect(canvasTab).toEqual(
      expect.objectContaining({
        kind: "canvas",
        cards: [{ sessionId: parentSession.id, x: 10, y: 20 }],
      }),
    );
    expect(sourceTab).toEqual(
      expect.objectContaining({
        kind: "source",
        originSessionId: null,
      }),
    );
  });

  it("preserves a pending delegated child session before the open can run", () => {
    const parentSession = makeSession("parent-session");
    const childSession = makeSession("child-session", "delegation-1");
    const workspace = makeSingleSessionWorkspace(childSession.id);
    const applyControlPanelLayout = vi.fn((nextWorkspace: WorkspaceState) => nextWorkspace);

    const result = reconcileAdoptedSessionsWorkspace({
      applyControlPanelLayout,
      canOpenPendingSession: false,
      current: workspace,
      mergedSessions: [parentSession, childSession],
      pendingOpenSessionId: childSession.id,
      pendingPaneId: null,
      pruneDelegatedChildWorkspaceTabs: true,
      sessionsChanged: false,
    });

    expect(result).toBe(workspace);
    expect(applyControlPanelLayout).not.toHaveBeenCalled();
  });

  it("returns the existing workspace when no adoption-side workspace work is needed", () => {
    const parentSession = makeSession("parent-session");
    const childSession = makeSession("child-session", "delegation-1");
    const workspace = makeSingleSessionWorkspace(parentSession.id);
    const applyControlPanelLayout = vi.fn((nextWorkspace: WorkspaceState) => nextWorkspace);

    const result = reconcileAdoptedSessionsWorkspace({
      applyControlPanelLayout,
      canOpenPendingSession: false,
      current: workspace,
      mergedSessions: [parentSession, childSession],
      pendingPaneId: null,
      pruneDelegatedChildWorkspaceTabs: true,
      sessionsChanged: false,
    });

    expect(result).toBe(workspace);
    expect(applyControlPanelLayout).not.toHaveBeenCalled();
  });

  it("does not open a session when pending opening is enabled without a target", () => {
    const parentSession = makeSession("parent-session");
    const workspace = makeSingleSessionWorkspace(parentSession.id);
    const applyControlPanelLayout = vi.fn((nextWorkspace: WorkspaceState) => nextWorkspace);

    const result = reconcileAdoptedSessionsWorkspace({
      applyControlPanelLayout,
      canOpenPendingSession: true,
      current: workspace,
      mergedSessions: [parentSession],
      pendingPaneId: null,
      pruneDelegatedChildWorkspaceTabs: false,
      sessionsChanged: false,
    });

    expect(result).toBe(workspace);
    expect(applyControlPanelLayout).not.toHaveBeenCalled();
  });

  it("opens a pending session after reconciling changed session state", () => {
    const parentSession = makeSession("parent-session");
    const openedSession = makeSession("opened-session");
    const workspace = makeSingleSessionWorkspace(parentSession.id);
    const applyControlPanelLayout = vi.fn((nextWorkspace: WorkspaceState) => nextWorkspace);

    const result = reconcileAdoptedSessionsWorkspace({
      applyControlPanelLayout,
      canOpenPendingSession: true,
      current: workspace,
      mergedSessions: [parentSession, openedSession],
      pendingOpenSessionId: openedSession.id,
      pendingPaneId: null,
      pruneDelegatedChildWorkspaceTabs: false,
      sessionsChanged: true,
    });

    expect(applyControlPanelLayout).toHaveBeenCalledTimes(2);
    expect(result.panes[0].activeSessionId).toBe(openedSession.id);
    expect(result.panes[0].tabs).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          kind: "session",
          sessionId: openedSession.id,
        }),
      ]),
    );
  });

  it("prunes delegated child tabs before opening a pending session", () => {
    const parentSession = makeSession("parent-session");
    const childSession = makeSession("child-session", "delegation-1");
    const openedSession = makeSession("opened-session");
    const workspace = makeWorkspaceWithTabs(
      [
        {
          id: `tab-${childSession.id}`,
          kind: "session",
          sessionId: childSession.id,
        },
        {
          id: `tab-${parentSession.id}`,
          kind: "session",
          sessionId: parentSession.id,
        },
      ],
      `tab-${childSession.id}`,
      childSession.id,
    );
    const applyControlPanelLayout = vi.fn((nextWorkspace: WorkspaceState) => nextWorkspace);

    const result = reconcileAdoptedSessionsWorkspace({
      applyControlPanelLayout,
      canOpenPendingSession: true,
      current: workspace,
      mergedSessions: [parentSession, childSession, openedSession],
      pendingOpenSessionId: openedSession.id,
      pendingPaneId: null,
      pruneDelegatedChildWorkspaceTabs: true,
      sessionsChanged: false,
    });

    const sessionTabIds = result.panes[0].tabs.flatMap((tab) =>
      tab.kind === "session" ? [tab.sessionId] : [],
    );
    expect(applyControlPanelLayout).toHaveBeenCalledTimes(2);
    expect(sessionTabIds).toContain(parentSession.id);
    expect(sessionTabIds).toContain(openedSession.id);
    expect(sessionTabIds).not.toContain(childSession.id);
    expect(result.panes[0].activeSessionId).toBe(openedSession.id);
  });
});
