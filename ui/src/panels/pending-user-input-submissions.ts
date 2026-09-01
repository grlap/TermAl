// Owns process-wide per-message user-input submission claims across direct and
// virtualized conversation rendering, session/pane remounts, and duplicate
// views of the same session. Does not own card-local draft state or API error
// presentation.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import { useCallback, useMemo, useSyncExternalStore } from "react";
import type { UserInputSubmitHandler } from "./virtualized-conversation-types";

const pendingSubmissions = new Map<string, Promise<void>>();
const pendingSubmissionListeners = new Set<() => void>();
let pendingSubmissionVersion = 0;

function emitPendingSubmissionChange() {
  pendingSubmissionVersion += 1;
  for (const listener of pendingSubmissionListeners) {
    listener();
  }
}

function subscribePendingSubmissions(listener: () => void) {
  pendingSubmissionListeners.add(listener);
  return () => pendingSubmissionListeners.delete(listener);
}

function pendingSubmissionsSnapshot() {
  return pendingSubmissionVersion;
}

function submissionKey(sessionId: string, messageId: string) {
  return `${sessionId}\0${messageId}`;
}

export function usePendingUserInputSubmissions(
  activeSessionId: string | null,
  onUserInputSubmit: UserInputSubmitHandler,
): {
  trackedUserInputSubmit: UserInputSubmitHandler;
  pendingUserInputMessageIds: ReadonlySet<string>;
} {
  const pendingVersion = useSyncExternalStore(
    subscribePendingSubmissions,
    pendingSubmissionsSnapshot,
    pendingSubmissionsSnapshot,
  );

  const pendingUserInputMessageIds = useMemo(() => {
    if (activeSessionId === null) {
      return new Set<string>();
    }
    const prefix = `${activeSessionId}\0`;
    return new Set(
      Array.from(pendingSubmissions.keys())
        .filter((key) => key.startsWith(prefix))
        .map((key) => key.slice(prefix.length)),
    );
  }, [activeSessionId, pendingVersion]);

  const trackedUserInputSubmit = useCallback<UserInputSubmitHandler>(
    (sessionId, messageId, answers) => {
      const key = submissionKey(sessionId, messageId);
      const existing = pendingSubmissions.get(key);
      if (existing) {
        return existing;
      }

      const settle = (trackedSubmission: Promise<void>) => {
        if (pendingSubmissions.get(key) !== trackedSubmission) {
          return;
        }
        pendingSubmissions.delete(key);
        emitPendingSubmissionChange();
      };
      const submission = Promise.resolve().then(() =>
        onUserInputSubmit(sessionId, messageId, answers),
      );
      let trackedSubmission: Promise<void>;
      trackedSubmission = submission.then(
        () => settle(trackedSubmission),
        () => {
          settle(trackedSubmission);
        },
      );
      pendingSubmissions.set(key, trackedSubmission);
      emitPendingSubmissionChange();
      return trackedSubmission;
    },
    [onUserInputSubmit],
  );

  return {
    trackedUserInputSubmit,
    pendingUserInputMessageIds,
  };
}
