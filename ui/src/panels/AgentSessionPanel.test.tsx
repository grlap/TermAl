import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  onRefreshSessionModelOptions = vi.fn(),
  onSend = vi.fn(),
  onSessionSettingsChange = vi.fn(),
}: {
  session: Session | null;
  committedDraft?: string;
  onDraftCommit?: (sessionId: string, nextValue: string) => void;
  onRefreshSessionModelOptions?: (sessionId: string) => void;
  onSend?: (sessionId: string, draftText?: string) => void;
  onSessionSettingsChange?: (sessionId: string, field: string, value: string) => void;
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
      isRefreshingModelOptions={false}
      onRefreshSessionModelOptions={onRefreshSessionModelOptions}
      onSend={onSend}
      onSessionSettingsChange={onSessionSettingsChange}
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

  it("expands /model from the slash command menu", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/" } });

    expect(screen.getByRole("option", { name: /\/model/i })).toBeInTheDocument();

    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(screen.getByLabelText("Message session-a")).toHaveValue("/model ");
  });

  it("applies a model slash command with keyboard navigation instead of sending a prompt", () => {
    const onSend = vi.fn();
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSend,
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith("session-a", "model", "gpt-5-mini");
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Message session-a")).toHaveValue("");
  });

  it("requests live Cursor model options when /model opens", async () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Cursor",
          cursorMode: "agent",
          model: "auto",
          modelOptions: undefined,
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    await waitFor(() => {
      expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
    });
  });
});
