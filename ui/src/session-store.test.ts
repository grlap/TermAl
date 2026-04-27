import { beforeEach, describe, expect, it } from "vitest";
import {
  getComposerSessionSnapshotForTesting,
  getSessionRecordSnapshotForTesting,
  getSessionSummarySnapshotForTesting,
  resetSessionStoreForTesting,
  syncComposerDraftForSession,
  syncComposerSessionsStore,
  syncComposerSessionsStoreIncremental,
  upsertSessionStoreSession,
} from "./session-store";
import type { DraftImageAttachment } from "./app-utils";
import type { Session, TextMessage } from "./types";

function createTextMessage(
  id: string,
  author: TextMessage["author"],
  text: string,
): TextMessage {
  return {
    author,
    id,
    text,
    timestamp: id,
    type: "text",
  };
}

function createSession(overrides: Partial<Session> = {}): Session {
  return {
    agent: "Codex",
    emoji: "🤖",
    id: "session-1",
    messages: [createTextMessage("user-1", "you", "First prompt")],
    model: "gpt-5.4",
    name: "Codex Session",
    preview: "",
    status: "idle",
    workdir: "C:/repo",
    ...overrides,
  };
}

function createDraftAttachment(
  overrides: Partial<DraftImageAttachment> = {},
): DraftImageAttachment {
  return {
    base64Data: "AAAA",
    byteSize: 12,
    fileName: "diagram.png",
    id: "attachment-1",
    mediaType: "image/png",
    previewUrl: "blob:diagram",
    ...overrides,
  };
}

describe("session-store composer snapshots", () => {
  beforeEach(() => {
    resetSessionStoreForTesting();
  });

  it("preserves snapshot identity when only assistant transcript messages change", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    const assistantOnlyUpdate = createSession({
      messages: [
        ...initialSession.messages,
        createTextMessage("assistant-1", "assistant", "Still working"),
      ],
      preview: "Still working",
    });
    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [assistantOnlyUpdate],
    });
    const secondSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    expect(secondSnapshot).toBe(firstSnapshot);
    expect(secondSnapshot?.promptHistory).toEqual(["First prompt"]);
  });

  it("updates prompt history when a new user prompt arrives", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    const updatedSession = createSession({
      messages: [
        ...initialSession.messages,
        createTextMessage("assistant-1", "assistant", "Done"),
        createTextMessage("user-2", "you", "Second prompt"),
      ],
    });
    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [updatedSession],
    });
    const secondSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    expect(secondSnapshot).not.toBe(firstSnapshot);
    expect(secondSnapshot?.promptHistory).toEqual(["First prompt", "Second prompt"]);
  });

  it("rebuilds prompt history when the transcript shrinks", () => {
    const initialSession = createSession({
      messages: [
        createTextMessage("user-1", "you", "First prompt"),
        createTextMessage("assistant-1", "assistant", "Done"),
        createTextMessage("user-2", "you", "Second prompt"),
      ],
    });

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    const shrunkSession = createSession({
      messages: [createTextMessage("user-1", "you", "First prompt")],
    });
    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [shrunkSession],
    });
    const secondSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    expect(firstSnapshot?.promptHistory).toEqual(["First prompt", "Second prompt"]);
    expect(secondSnapshot).not.toBe(firstSnapshot);
    expect(secondSnapshot?.promptHistory).not.toBe(firstSnapshot?.promptHistory);
    expect(secondSnapshot?.promptHistory).toEqual(["First prompt"]);
  });

  it("rebuilds prompt history when the previous boundary message changes", () => {
    const initialSession = createSession({
      messages: [
        createTextMessage("user-1", "you", "First prompt"),
        createTextMessage("assistant-1", "assistant", "Done"),
      ],
    });

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    const rewrittenBoundarySession = createSession({
      messages: [
        createTextMessage("user-1", "you", "First prompt"),
        createTextMessage("user-2", "you", "Second prompt"),
      ],
    });
    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [rewrittenBoundarySession],
    });
    const secondSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    expect(firstSnapshot?.promptHistory).toEqual(["First prompt"]);
    expect(secondSnapshot).not.toBe(firstSnapshot);
    expect(secondSnapshot?.promptHistory).not.toBe(firstSnapshot?.promptHistory);
    expect(secondSnapshot?.promptHistory).toEqual(["First prompt", "Second prompt"]);
  });

  it("rebuilds prompt history when a same-length update turns the last message into a user prompt", () => {
    const initialSession = createSession({
      messages: [
        createTextMessage("user-1", "you", "First prompt"),
        createTextMessage("message-2", "assistant", "Done"),
      ],
    });

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    const userBoundarySession = createSession({
      messages: [
        createTextMessage("user-1", "you", "First prompt"),
        createTextMessage("message-2", "you", "Second prompt"),
      ],
    });
    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [userBoundarySession],
    });
    const secondSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    expect(firstSnapshot?.promptHistory).toEqual(["First prompt"]);
    expect(secondSnapshot).not.toBe(firstSnapshot);
    expect(secondSnapshot?.promptHistory).not.toBe(firstSnapshot?.promptHistory);
    expect(secondSnapshot?.promptHistory).toEqual(["First prompt", "Second prompt"]);
  });

  it("tracks draft and attachment changes without replacing unchanged prompt history", () => {
    const initialSession = createSession();
    const attachment = createDraftAttachment();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {
        [initialSession.id]: [attachment],
      },
      draftsBySessionId: {
        [initialSession.id]: "Draft update",
      },
      sessions: [initialSession],
    });
    const secondSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    expect(secondSnapshot).not.toBe(firstSnapshot);
    expect(secondSnapshot?.committedDraft).toBe("Draft update");
    expect(secondSnapshot?.draftAttachments).toHaveLength(1);
    expect(secondSnapshot?.promptHistory).toBe(firstSnapshot?.promptHistory);
  });

  it("incrementally preserves prompt history identity for assistant-only transcript churn", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    const assistantOnlyUpdate = createSession({
      messages: [
        ...initialSession.messages,
        createTextMessage("assistant-1", "assistant", "Still working"),
      ],
      preview: "Still working",
    });
    syncComposerSessionsStoreIncremental({
      changedSessions: [assistantOnlyUpdate],
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
    });
    const secondSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);

    expect(secondSnapshot).toBe(firstSnapshot);
    expect(secondSnapshot?.promptHistory).toEqual(["First prompt"]);
  });

  it("patches only the targeted draft slice without replacing the session record", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);
    const firstRecord = getSessionRecordSnapshotForTesting(initialSession.id);

    syncComposerDraftForSession({
      sessionId: initialSession.id,
      committedDraft: "Patched draft",
      draftAttachments: [],
    });

    const secondSnapshot = getComposerSessionSnapshotForTesting(initialSession.id);
    const secondRecord = getSessionRecordSnapshotForTesting(initialSession.id);
    expect(secondSnapshot).not.toBe(firstSnapshot);
    expect(secondSnapshot?.committedDraft).toBe("Patched draft");
    expect(secondRecord).toBe(firstRecord);
  });

  it("prunes removed sessions from the composer slice", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    expect(getComposerSessionSnapshotForTesting(initialSession.id)).not.toBeNull();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [],
    });

    expect(getComposerSessionSnapshotForTesting(initialSession.id)).toBeNull();
  });
});

describe("session-store summary snapshots", () => {
  beforeEach(() => {
    resetSessionStoreForTesting();
  });

  it("preserves summary identity when only transcript fields change", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSummary = getSessionSummarySnapshotForTesting(initialSession.id);

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [
        createSession({
          messages: [
            ...initialSession.messages,
            createTextMessage("assistant-1", "assistant", "Streaming"),
          ],
          pendingPrompts: [
            {
              id: "pending-1",
              text: "Queued prompt",
              timestamp: "pending-1",
            },
          ],
          preview: "Streaming",
        }),
      ],
    });
    const secondSummary = getSessionSummarySnapshotForTesting(initialSession.id);

    expect(secondSummary).toBe(firstSummary);
  });

  it("updates the summary when a summary-visible field changes", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstSummary = getSessionSummarySnapshotForTesting(initialSession.id);

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [
        createSession({
          status: "active",
        }),
      ],
    });
    const secondSummary = getSessionSummarySnapshotForTesting(initialSession.id);

    expect(secondSummary).not.toBe(firstSummary);
    expect(secondSummary?.status).toBe("active");
  });
});

describe("session-store record snapshots", () => {
  beforeEach(() => {
    resetSessionStoreForTesting();
  });

  it("preserves record identity when the session object itself is unchanged", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstRecord = getSessionRecordSnapshotForTesting(initialSession.id);

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {
        [initialSession.id]: [createDraftAttachment()],
      },
      draftsBySessionId: {
        [initialSession.id]: "Draft update",
      },
      sessions: [initialSession],
    });
    const secondRecord = getSessionRecordSnapshotForTesting(initialSession.id);

    expect(secondRecord).toBe(firstRecord);
  });

  it("replaces the record when a new session object is adopted", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    const firstRecord = getSessionRecordSnapshotForTesting(initialSession.id);

    const updatedSession = createSession({
      messages: [
        ...initialSession.messages,
        createTextMessage("assistant-1", "assistant", "Streaming"),
      ],
      preview: "Streaming",
    });
    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [updatedSession],
    });
    const secondRecord = getSessionRecordSnapshotForTesting(initialSession.id);

    expect(secondRecord).toBe(updatedSession);
    expect(secondRecord).not.toBe(firstRecord);
  });

  it("upserts a single session without replacing unrelated record slices", () => {
    const firstSession = createSession();
    const secondSession = createSession({
      id: "session-2",
      name: "Other session",
    });

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [firstSession, secondSession],
    });
    const firstRecord = getSessionRecordSnapshotForTesting(firstSession.id);
    const secondRecord = getSessionRecordSnapshotForTesting(secondSession.id);

    const updatedSecondSession = {
      ...secondSession,
      status: "active" as const,
    };
    upsertSessionStoreSession({
      session: updatedSecondSession,
      committedDraft: "",
      draftAttachments: [],
    });

    expect(getSessionRecordSnapshotForTesting(firstSession.id)).toBe(firstRecord);
    expect(getSessionRecordSnapshotForTesting(secondSession.id)).toBe(
      updatedSecondSession,
    );
    expect(getSessionRecordSnapshotForTesting(secondSession.id)).not.toBe(
      secondRecord,
    );
  });

  it("incrementally updates only the targeted session slices", () => {
    const firstSession = createSession();
    const secondSession = createSession({
      id: "session-2",
      name: "Other session",
    });

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [firstSession, secondSession],
    });
    const firstRecord = getSessionRecordSnapshotForTesting(firstSession.id);
    const firstSummary = getSessionSummarySnapshotForTesting(firstSession.id);
    const secondRecord = getSessionRecordSnapshotForTesting(secondSession.id);

    const updatedSecondSession = {
      ...secondSession,
      status: "active" as const,
    };
    syncComposerSessionsStoreIncremental({
      changedSessions: [updatedSecondSession],
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
    });

    expect(getSessionRecordSnapshotForTesting(firstSession.id)).toBe(firstRecord);
    expect(getSessionSummarySnapshotForTesting(firstSession.id)).toBe(firstSummary);
    expect(getSessionRecordSnapshotForTesting(secondSession.id)).not.toBe(secondRecord);
    expect(getSessionRecordSnapshotForTesting(secondSession.id)).toBe(
      updatedSecondSession,
    );
    expect(getSessionSummarySnapshotForTesting(secondSession.id)?.status).toBe(
      "active",
    );
  });

  it("prunes removed sessions from the record slice", () => {
    const initialSession = createSession();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [initialSession],
    });
    expect(getSessionRecordSnapshotForTesting(initialSession.id)).not.toBeNull();

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [],
    });

    expect(getSessionRecordSnapshotForTesting(initialSession.id)).toBeNull();
  });

  it("incrementally prunes removed sessions from every store slice", () => {
    const firstSession = createSession();
    const secondSession = createSession({
      id: "session-2",
      name: "Other session",
    });

    syncComposerSessionsStore({
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      sessions: [firstSession, secondSession],
    });

    syncComposerSessionsStoreIncremental({
      changedSessions: [],
      draftAttachmentsBySessionId: {},
      draftsBySessionId: {},
      removedSessionIds: [secondSession.id],
    });

    expect(getComposerSessionSnapshotForTesting(firstSession.id)).not.toBeNull();
    expect(getSessionRecordSnapshotForTesting(firstSession.id)).not.toBeNull();
    expect(getSessionSummarySnapshotForTesting(firstSession.id)).not.toBeNull();
    expect(getComposerSessionSnapshotForTesting(secondSession.id)).toBeNull();
    expect(getSessionRecordSnapshotForTesting(secondSession.id)).toBeNull();
    expect(getSessionSummarySnapshotForTesting(secondSession.id)).toBeNull();
  });
});
