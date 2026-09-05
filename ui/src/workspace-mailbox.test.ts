import { describe, expect, it } from "vitest";

import {
  activatePane,
  openMailboxInWorkspaceState,
} from "./workspace";
import { type WorkspaceState } from "./workspace-types";
import { isWorkspaceTab } from "./workspace-tab-validation";

function workspace(): WorkspaceState {
  return {
    lastContentPaneId: null,
    lastViewerPaneId: null,
    root: { type: "pane", paneId: "pane-1" },
    panes: [
      {
        id: "pane-1",
        tabs: [
          {
            id: "session-tab",
            kind: "session",
            sessionId: "session-codex",
          },
        ],
        activeTabId: "session-tab",
        activeSessionId: "session-codex",
        viewMode: "session",
        lastSessionViewMode: "session",
        sourcePath: null,
      },
    ],
    activePaneId: "pane-1",
  };
}

describe("mailbox workspace tabs", () => {
  it("validates the durable mailbox identity needed for restored tabs", () => {
    expect(
      isWorkspaceTab({
        id: "mailbox-tab",
        kind: "mailbox",
        mailboxId: "mailbox-1",
        originSessionId: "session-codex",
        refreshToken: "refresh-1",
      }),
    ).toBe(true);
    expect(
      isWorkspaceTab({
        id: "mailbox-tab",
        kind: "mailbox",
        mailboxId: "mailbox-1",
        originSessionId: "session-codex",
      }),
    ).toBe(false);
  });

  it("focuses one existing tab when repeated notification cards open the same mailbox", () => {
    const opened = openMailboxInWorkspaceState(
      workspace(),
      "mailbox-1",
      "pane-1",
      "session-codex",
      "project-1",
    );
    const mailboxTab = opened.panes[0]?.tabs.find(
      (tab) => tab.kind === "mailbox",
    );
    expect(mailboxTab).toBeTruthy();

    const returnedToSession = activatePane(opened, "pane-1", "session-tab");
    const afterThreeCards = [1, 2].reduce(
      (current) =>
        openMailboxInWorkspaceState(
          current,
          "mailbox-1",
          "pane-1",
          "session-codex",
          "project-1",
        ),
      returnedToSession,
    );

    expect(
      afterThreeCards.panes.flatMap((pane) =>
        pane.tabs.filter((tab) => tab.kind === "mailbox"),
      ),
    ).toHaveLength(1);
    expect(afterThreeCards.panes[0]?.activeTabId).toBe(mailboxTab?.id);
    expect(afterThreeCards.panes[0]?.tabVisitHistory).toEqual([
      mailboxTab?.id,
      "session-tab",
    ]);
    const refreshedMailboxTab = afterThreeCards.panes[0]?.tabs.find(
      (tab) => tab.kind === "mailbox",
    );
    expect(
      refreshedMailboxTab?.kind === "mailbox"
        ? refreshedMailboxTab.refreshToken
        : null,
    ).not.toBe(mailboxTab?.kind === "mailbox" ? mailboxTab.refreshToken : null);
  });
});
