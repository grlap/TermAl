// Exercises streaming classification through the production publication paths
// and Markdown card, including completed -> next turn and card remounts.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MessageCard } from "./message-cards";
import { streamingAssistantTextMessageIdForSession } from "./SessionPaneView.render-callbacks";
import {
  getSessionRecordSnapshotForTesting,
  removeSessionFromStore,
  resetSessionStoreForTesting,
  syncComposerSessionsStore,
  syncComposerSessionsStoreIncremental,
  upsertSessionStoreSession,
  useSessionRecordSnapshot,
} from "./session-store";
import type { Session, TextMessage } from "./types";

const answer: TextMessage = {
  id: "answer-1", author: "assistant", type: "text", timestamp: "10:00",
  text: "Summary\n\n| Name | Value |\n| --- | --- |\n| One | Two |\n\n**Done.**",
};
function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-1", name: "Session", emoji: "", agent: "Codex", workdir: "/repo",
    model: "test", status: "idle", preview: "", messages: [answer], ...overrides,
  };
}
const publications = {
  full: (value: Session) => syncComposerSessionsStore({
    sessions: [value], draftsBySessionId: {}, draftAttachmentsBySessionId: {},
  }),
  incremental: (value: Session) => syncComposerSessionsStoreIncremental({
    changedSessions: [value], draftsBySessionId: {}, draftAttachmentsBySessionId: {},
  }),
  upsert: (value: Session) => upsertSessionStoreSession({
    session: value, committedDraft: "", draftAttachments: [],
  }),
};
const queuedPrompt = { id: "prompt-2", timestamp: "10:01", text: "Next", localOnly: true };

function Conversation() {
  const current = useSessionRecordSnapshot("session-1");
  const streamingId = streamingAssistantTextMessageIdForSession(current);
  return current?.messages.map((message) => (
    <MessageCard key={message.id} message={message}
      isStreamingAssistantTextMessage={message.id === streamingId}
      onApprovalDecision={vi.fn()} onUserInputSubmit={async () => {}} />
  ));
}

beforeEach(resetSessionStoreForTesting);
afterEach(() => { cleanup(); resetSessionStoreForTesting(); });

describe.each(Object.entries(publications))("streaming text via %s publication", (_name, publish) => {
  it("keeps a completed table rendered after the next prompt, including a card remount", () => {
    publish(session({ status: "active" }));
    const view = render(<Conversation />);
    expect(screen.getByText("Response still streaming…")).toBeInTheDocument();
    act(() => publish(session()));
    const table = screen.getByRole("table");
    expect(screen.queryByText("Response still streaming…")).not.toBeInTheDocument();

    act(() => publish(session({ status: "active", pendingPrompts: [queuedPrompt] })));
    expect(screen.getByRole("table")).toBe(table);
    expect(screen.queryByText("Response still streaming…")).not.toBeInTheDocument();
    view.unmount();
    render(<Conversation />);
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.queryByText("Response still streaming…")).not.toBeInTheDocument();

    act(() => publish(session({ status: "active", messages: [
      answer, { ...answer, id: "answer-2", text: "Next\n\n| A | B |" },
    ] })));
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByText("Response still streaming…")).toBeInTheDocument();
  });

  it("does not turn off a genuine stream just because another prompt is queued", () => {
    publish(session({ status: "active" }));
    publish(session({ status: "active", pendingPrompts: [queuedPrompt] }));
    expect(streamingAssistantTextMessageIdForSession(getSessionRecordSnapshotForTesting("session-1")))
      .toBe(answer.id);
  });

  it("can stream new content on a reused ID without changing old snapshot evidence", () => {
    const settled = session();
    publish(settled);
    const waiting = session({ status: "active" });
    publish(waiting);
    const changed = session({ status: "active", messages: [{ ...answer, text: `${answer.text}\nMore` }] });
    publish(changed);
    expect(streamingAssistantTextMessageIdForSession(changed)).toBe(answer.id);
    expect(streamingAssistantTextMessageIdForSession(waiting)).toBeNull();
  });

  it("streams a different message even when its text matches the completed answer", () => {
    publish(session());
    const next = session({ status: "active", messages: [{ ...answer, id: "answer-2" }] });
    publish(next);
    expect(streamingAssistantTextMessageIdForSession(next)).toBe("answer-2");
  });

  it("retains completion through an empty resident window and immutable hydration clones", () => {
    publish(session());
    publish(session({ status: "active", messages: [], messageCount: 1, messagesLoaded: false }));
    const hydrated = session({ status: "active", messages: [{ ...answer }] });
    publish(hydrated);
    expect(streamingAssistantTextMessageIdForSession(hydrated)).toBeNull();
  });
});

it("keeps the live-tail fallback when the first snapshot is already active", () => {
  const initial = session({ status: "active", pendingPrompts: [queuedPrompt] });
  publications.upsert(initial);
  // There is no terminal snapshot and no wire turn ID to prove completion.
  expect(streamingAssistantTextMessageIdForSession(initial)).toBe(answer.id);
});

it.each([
  { hasNewerHistory: true },
  { messageStartIndex: 20, messageCount: 30 },
])("does not stream the last resident item of a historical window: %j", (window) => {
  expect(streamingAssistantTextMessageIdForSession(session({ status: "active", ...window }))).toBeNull();
});

it("does not leak completion evidence into another session or a removed store record", () => {
  publications.upsert(session());
  const other = session({ id: "session-2", status: "active" });
  publications.upsert(other);
  expect(streamingAssistantTextMessageIdForSession(other)).toBe(answer.id);
  removeSessionFromStore({ sessionId: "session-1" });
  const replacement = session({ status: "active" });
  publications.upsert(replacement);
  expect(streamingAssistantTextMessageIdForSession(replacement)).toBe(answer.id);
});
