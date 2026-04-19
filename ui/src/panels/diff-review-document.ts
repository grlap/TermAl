// Pure helpers that build and extend the `ReviewDocument` shape the
// diff panel persists alongside each change-set review. These
// functions have no React, Monaco, or DOM dependency — they take a
// document (or null) plus a few context fields and return the
// next-state document.
//
// What this file owns:
//   - `ReviewOriginContext` — the tuple of agent name, session id,
//     workdir, and anchor message id needed to stamp a new review
//     document's `origin` block.
//   - `ensureReviewDocument` — idempotent "get or create" that
//     either augments an existing review (adding the current file
//     path to `files` if it isn't already listed) or builds a fresh
//     review (`version: 1`, `revision: 0`, empty threads, initial
//     `files` entry for the visible file) stamped with the origin
//     session / message.
//   - `ensureReviewFiles` — the inner helper that adds a
//     `{ filePath, changeType }` entry to the `files` array when
//     the path isn't already listed.
//   - `createReviewComment` — builds a new user-authored review
//     comment with a `comment-${uuid}` id and matching
//     `createdAt` / `updatedAt` ISO timestamps.
//   - `buildReviewHandoffPrompt` — formats the agent hand-off
//     prompt that gets copied when the user asks an agent to
//     address open threads in a review file.
//
// What this file does NOT own:
//   - Persistence — `fetchReviewDocument` / `saveReviewDocument`
//     live in `../api` and stay there. This module only produces
//     the document payload; the caller decides when to save.
//   - Thread / comment editing UI, open-count aggregation, or
//     thread status transitions — those stay in `./DiffPanel.tsx`
//     alongside the React components that drive them.
//   - The `ReviewState` state wrapper (status / error / loading
//     flags around a `ReviewDocument | null`) — that stays with
//     the panel since it is React-state-shaped.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same document shape,
// same `version` / `revision` defaults, same comment id format.

import type { ReviewComment, ReviewDocument, ReviewThread } from "../api";
import type { DiffMessage } from "../types";

export type ReviewOriginContext = {
  agentName: string | null;
  messageId: string;
  sessionId: string | null;
  workdir: string | null;
};

export function ensureReviewDocument(
  review: ReviewDocument | null,
  changeSetId: string,
  context: {
    changeType: DiffMessage["changeType"];
    filePath: string | null;
    origin: ReviewOriginContext;
  },
): ReviewDocument {
  if (review) {
    return {
      ...review,
      files: ensureReviewFiles(review.files ?? [], context.filePath, context.changeType),
      threads: review.threads ?? [],
    };
  }

  return {
    version: 1,
    revision: 0,
    changeSetId,
    origin:
      context.origin.sessionId &&
      context.origin.workdir &&
      context.origin.agentName
        ? {
            sessionId: context.origin.sessionId,
            messageId: context.origin.messageId,
            agent: context.origin.agentName,
            workdir: context.origin.workdir,
            createdAt: new Date().toISOString(),
          }
        : null,
    files: ensureReviewFiles([], context.filePath, context.changeType),
    threads: [],
  };
}

export function ensureReviewFiles(
  files: NonNullable<ReviewDocument["files"]>,
  filePath: string | null,
  changeType: DiffMessage["changeType"],
) {
  if (!filePath || files.some((file) => file.filePath === filePath)) {
    return files;
  }

  return [...files, { filePath, changeType }];
}

export function createReviewComment(body: string): ReviewComment {
  const timestamp = new Date().toISOString();
  return {
    id: `comment-${crypto.randomUUID()}`,
    author: "user",
    body: body.trim(),
    createdAt: timestamp,
    updatedAt: timestamp,
  };
}

export function buildReviewHandoffPrompt(reviewFilePath: string, threads: ReviewThread[]) {
  const openThreads = threads.filter((thread) => thread.status === "open").length;
  return openThreads > 0
    ? `Please address the ${openThreads} open review thread${openThreads === 1 ? "" : "s"} in ${reviewFilePath}. Reply in each thread and resolve threads you have handled.`
    : `Review file: ${reviewFilePath}`;
}
