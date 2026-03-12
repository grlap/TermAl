import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AgentSessionPanelFooter } from "./AgentSessionPanel";
import type { Session } from "../types";

function makeSession(id: string, overrides?: Partial<Session>): Session {
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
    ...overrides,
  };
}

function renderFooter({
  session,
  committedDraft = "",
  onDraftCommit = vi.fn(),
}: {
  session: Session | null;
  committedDraft?: string;
  onDraftCommit?: (sessionId: string, nextValue: string) => void;
}) {
  return (
    <AgentSessionPanelFooter
      paneId="pane-1"
      viewMode="session"
      activeSession={session}
      committedDraft={committedDraft}
      draftAttachments={[]}
      formatByteSize={(byteSize) => `${byteSize} B`}
      isSending={false}
      isStopping={false}
      isSessionBusy={false}
      showNewResponseIndicator={false}
      footerModeLabel="Session"
      onScrollToLatest={() => {}}
      onDraftCommit={onDraftCommit}
      onDraftAttachmentRemove={() => {}}
      onSend={() => {}}
      onStopSession={() => {}}
      onPaste={() => {}}
    />
  );
}

describe("AgentSessionPanelFooter", () => {
  it("does not commit a draft during unrelated session rerenders", () => {
    const initialCommit = vi.fn();
    const nextCommit = vi.fn();
    const sessionId = "session-a";
    const { rerender } = render(
      renderFooter({
        onDraftCommit: initialCommit,
        session: makeSession(sessionId, { preview: "first preview" }),
      }),
    );

    const textarea = screen.getByLabelText(`Message ${sessionId}`);
    fireEvent.change(textarea, { target: { value: "draft in progress" } });

    rerender(
      renderFooter({
        onDraftCommit: nextCommit,
        session: makeSession(sessionId, { preview: "streamed preview", status: "active" }),
      }),
    );

    expect(initialCommit).not.toHaveBeenCalled();
    expect(nextCommit).not.toHaveBeenCalled();
    expect(screen.getByLabelText(`Message ${sessionId}`)).toHaveValue("draft in progress");
  });

  it("commits the in-progress draft when switching sessions", () => {
    const onDraftCommit = vi.fn();
    const { rerender } = render(
      renderFooter({
        onDraftCommit,
        session: makeSession("session-a"),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "carry this draft" },
    });

    rerender(
      renderFooter({
        onDraftCommit,
        session: makeSession("session-b"),
      }),
    );

    expect(onDraftCommit).toHaveBeenCalledWith("session-a", "carry this draft");
  });
});
