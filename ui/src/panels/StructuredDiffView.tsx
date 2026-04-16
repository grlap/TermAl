import { Fragment, useState, type ReactNode } from "react";
import type { ReviewAnchor, ReviewThread } from "../api";
import {
  type DiffPreviewCell,
  type DiffPreviewHighlight,
  type DiffPreviewHunk,
  type DiffPreviewModel,
  type DiffPreviewRow,
} from "../diff-preview";

export function StructuredDiffView({
  filePath,
  preview,
  scrollRef,
  threads = [],
  isSavingReview = false,
  onCreateThread,
  onReplyToThread,
  onUpdateThreadStatus,
}: {
  filePath: string | null;
  preview: DiffPreviewModel;
  scrollRef?: { current: HTMLDivElement | null };
  threads?: ReviewThread[];
  isSavingReview?: boolean;
  onCreateThread?: (anchor: ReviewAnchor, body: string) => Promise<void>;
  onReplyToThread?: (threadId: string, body: string) => Promise<void>;
  onUpdateThreadStatus?: (threadId: string, status: ReviewThread["status"]) => Promise<void>;
}) {
  const [draftBody, setDraftBody] = useState("");
  const [composer, setComposer] = useState<ReviewComposerState | null>(null);
  const [pendingActionKey, setPendingActionKey] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ReviewActionError | null>(null);
  const changeSetThreads = threads.filter((thread) => thread.anchor.kind === "changeSet");
  const fileThreads = filePath
    ? threads.filter(
        (thread) => thread.anchor.kind === "file" && thread.anchor.filePath === filePath,
      )
    : [];
  const canCreateThread = Boolean(onCreateThread);
  const canReplyToThread = Boolean(onReplyToThread);
  const canUpdateThreadStatus = Boolean(onUpdateThreadStatus);

  function openThreadComposer(anchor: ReviewAnchor) {
    if (!onCreateThread) {
      return;
    }

    setActionError(null);
    setDraftBody("");
    setComposer({
      mode: "newThread",
      key: reviewAnchorKey(anchor),
      anchor,
    });
  }

  function openReplyComposer(threadId: string) {
    if (!onReplyToThread) {
      return;
    }

    setActionError(null);
    setDraftBody("");
    setComposer({
      mode: "reply",
      key: threadId,
      threadId,
    });
  }

  function closeComposer() {
    setComposer(null);
    setDraftBody("");
    setActionError(null);
  }

  async function handleComposerSubmit() {
    if (!composer) {
      return;
    }

    const body = draftBody.trim();
    if (!body) {
      setActionError({
        key: composer.key,
        message: "Enter a review comment before submitting.",
      });
      return;
    }

    setPendingActionKey(composer.key);
    setActionError(null);
    try {
      if (composer.mode === "newThread") {
        await onCreateThread?.(composer.anchor, body);
      } else {
        await onReplyToThread?.(composer.threadId, body);
      }
      closeComposer();
    } catch (error) {
      setActionError({
        key: composer.key,
        message: getActionErrorMessage(error),
      });
    } finally {
      setPendingActionKey(null);
    }
  }

  async function handleThreadStatusToggle(thread: ReviewThread) {
    if (!onUpdateThreadStatus) {
      return;
    }

    const nextStatus = thread.status === "open" ? "resolved" : "open";
    setPendingActionKey(thread.id);
    setActionError(null);
    try {
      await onUpdateThreadStatus(thread.id, nextStatus);
    } catch (error) {
      setActionError({
        key: thread.id,
        message: getActionErrorMessage(error),
      });
    } finally {
      setPendingActionKey(null);
    }
  }

  function renderComposer(targetKey: string, contextLabel: string) {
    if (composer?.key !== targetKey) {
      return null;
    }

    const isSubmitting = isSavingReview || pendingActionKey === targetKey;
    return (
      <ReviewComposer
        body={draftBody}
        contextLabel={contextLabel}
        error={actionError?.key === targetKey ? actionError.message : null}
        isSubmitting={isSubmitting}
        mode={composer.mode}
        onBodyChange={setDraftBody}
        onCancel={closeComposer}
        onSubmit={() => void handleComposerSubmit()}
      />
    );
  }

  function renderThreadList(label: string, sectionThreads: ReviewThread[]) {
    if (sectionThreads.length === 0) {
      return null;
    }

    return (
      <div className="diff-review-thread-list" aria-label={label}>
        {sectionThreads.map((thread) => {
          const isReplyComposerOpen = composer?.mode === "reply" && composer.threadId === thread.id;
          const isPending = isSavingReview || pendingActionKey === thread.id;
          const statusActionLabel = thread.status === "open" ? "Resolve" : "Reopen";

          return (
            <article key={thread.id} className="diff-review-thread">
              <header className="diff-review-thread-header">
                <span className={`chip diff-review-thread-status diff-review-thread-status-${thread.status}`}>
                  {thread.status}
                </span>
                <span className="diff-review-thread-count">
                  {`${thread.comments.length} comment${thread.comments.length === 1 ? "" : "s"}`}
                </span>
              </header>
              <div className="diff-review-thread-comments">
                {thread.comments.map((comment) => (
                  <div key={comment.id} className="diff-review-thread-comment">
                    <div className="diff-review-thread-meta">
                      <strong>{comment.author === "agent" ? "Agent" : "User"}</strong>
                      <span>{comment.createdAt}</span>
                    </div>
                    <p className="diff-review-thread-body">{comment.body}</p>
                  </div>
                ))}
              </div>
              {canReplyToThread || canUpdateThreadStatus ? (
                <div className="diff-review-thread-actions">
                  {canReplyToThread ? (
                    <button
                      className="ghost-button diff-review-action-button"
                      type="button"
                      onClick={() => openReplyComposer(thread.id)}
                      disabled={isSavingReview}
                    >
                      Reply
                    </button>
                  ) : null}
                  {canUpdateThreadStatus ? (
                    <button
                      className="ghost-button diff-review-action-button"
                      type="button"
                      onClick={() => void handleThreadStatusToggle(thread)}
                      disabled={isPending}
                    >
                      {isPending ? "Saving..." : statusActionLabel}
                    </button>
                  ) : null}
                </div>
              ) : null}
              {actionError?.key === thread.id ? (
                <p className="support-copy diff-preview-note">{actionError.message}</p>
              ) : null}
              {isReplyComposerOpen ? renderComposer(thread.id, "Reply to thread") : null}
            </article>
          );
        })}
      </div>
    );
  }

  function renderAnchorSection({
    anchor,
    buttonLabel,
    contextLabel,
    sectionKey,
    sectionLabel,
    sectionThreads,
    title,
  }: {
    anchor: ReviewAnchor;
    buttonLabel: string;
    contextLabel: string;
    sectionKey: string;
    sectionLabel: string;
    sectionThreads: ReviewThread[];
    title: string;
  }) {
    if (sectionThreads.length === 0 && !canCreateThread && composer?.key !== sectionKey) {
      return null;
    }

    return (
      <section className="diff-review-section">
        <header className="diff-review-section-header">
          <div>
            <strong>{title}</strong>
            {sectionThreads.length > 0 ? (
              <span className="support-copy diff-review-section-count">
                {`${sectionThreads.length} thread${sectionThreads.length === 1 ? "" : "s"}`}
              </span>
            ) : null}
          </div>
          {canCreateThread ? (
            <button
              className="ghost-button diff-review-action-button"
              type="button"
              onClick={() => openThreadComposer(anchor)}
              disabled={isSavingReview}
            >
              {buttonLabel}
            </button>
          ) : null}
        </header>
        {renderComposer(sectionKey, contextLabel)}
        {renderThreadList(sectionLabel, sectionThreads)}
      </section>
    );
  }

  return (
    <div className="diff-editor-shell structured-diff-shell">
      <div className="structured-diff">
        <div className="structured-diff-column-headings" aria-hidden="true">
          <div className="structured-diff-column-heading">
            <span className="structured-diff-column-eyebrow">Original</span>
            <strong>{filePath ?? "Previous version"}</strong>
          </div>
          <div className="structured-diff-column-heading">
            <span className="structured-diff-column-eyebrow">Updated</span>
            <strong>{filePath ?? "Patched version"}</strong>
          </div>
        </div>

        <div
          className="structured-diff-body"
          data-testid="structured-diff-view"
          ref={scrollRef}
        >
          {renderAnchorSection({
            anchor: { kind: "changeSet" },
            buttonLabel: "Comment on change set",
            contextLabel: "Add a change-set review comment",
            sectionKey: reviewAnchorKey({ kind: "changeSet" }),
            sectionLabel: "Change-set review threads",
            sectionThreads: changeSetThreads,
            title: "Change set review",
          })}
          {filePath
            ? renderAnchorSection({
                anchor: { kind: "file", filePath },
                buttonLabel: "Comment on file",
                contextLabel: "Add a file review comment",
                sectionKey: reviewAnchorKey({ kind: "file", filePath }),
                sectionLabel: "File review threads",
                sectionThreads: fileThreads,
                title: "File review",
              })
            : null}
          {preview.hunks.map((hunk, index) => (
            <section
              key={`${index}:${hunk.header ?? "patch"}`}
              className="structured-diff-hunk"
            >
              <header className="structured-diff-hunk-header">
                <div className="structured-diff-hunk-meta">
                  <span className="structured-diff-hunk-badge">{`Hunk ${index + 1}`}</span>
                  <code>{formatHunkLabel(hunk)}</code>
                </div>
                {canCreateThread && filePath && hunk.header ? (
                  <button
                    className="ghost-button diff-review-action-button"
                    type="button"
                    onClick={() =>
                      openThreadComposer({
                        kind: "hunk",
                        filePath,
                        hunkHeader: hunk.header!,
                      })
                    }
                    disabled={isSavingReview}
                  >
                    Comment on hunk
                  </button>
                ) : null}
              </header>
              {filePath && hunk.header ? renderComposer(reviewAnchorKey({
                kind: "hunk",
                filePath,
                hunkHeader: hunk.header!,
              }), `Add a review comment for ${hunk.header}`) : null}
              {filePath && hunk.header ? (
                renderThreadList(
                  `Review threads for ${hunk.header}`,
                  threads.filter(
                    (thread) =>
                      thread.anchor.kind === "hunk" &&
                      thread.anchor.filePath === filePath &&
                      thread.anchor.hunkHeader === hunk.header,
                  ),
                )
              ) : null}
              <div
                className="structured-diff-grid"
                role="table"
                aria-label={filePath ? `Diff preview for ${filePath}` : "Diff preview"}
              >
                {hunk.rows.map((row, rowIndex) => {
                  const lineAnchor =
                    filePath && hunk.header
                      ? createLineAnchor(filePath, hunk.header!, row)
                      : null;
                  const lineThreads =
                    filePath && hunk.header && row.kind !== "omitted"
                      ? threads.filter((thread) =>
                          matchesLineThread(thread, filePath, hunk.header ?? null, row),
                        )
                      : [];
                  const lineComposerKey = lineAnchor ? reviewAnchorKey(lineAnchor) : null;
                  const shouldRenderThreadRow =
                    lineThreads.length > 0 ||
                    (lineComposerKey !== null && composer?.key === lineComposerKey);

                  return (
                    <Fragment key={`${index}:${rowIndex}:${row.kind}:${row.left.text}:${row.right.text}`}>
                      <StructuredDiffRow
                        row={row}
                        commentAction={
                          lineAnchor && canCreateThread
                            ? {
                                disabled: isSavingReview,
                                label: `Comment on ${formatLineAnchorLabel(lineAnchor)}`,
                                onClick: () => openThreadComposer(lineAnchor),
                              }
                            : null
                        }
                      />
                      {shouldRenderThreadRow ? (
                        <ReviewThreadRow>
                          {lineComposerKey ? renderComposer(lineComposerKey, "Add a line review comment") : null}
                          {renderThreadList("Line review threads", lineThreads)}
                        </ReviewThreadRow>
                      ) : null}
                    </Fragment>
                  );
                })}
              </div>
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}

function StructuredDiffRow({
  row,
  commentAction = null,
}: {
  row: DiffPreviewRow;
  commentAction?: {
    disabled: boolean;
    label: string;
    onClick: () => void;
  } | null;
}) {
  if (row.kind === "omitted") {
    return (
      <div className="structured-diff-row structured-diff-row-omitted" role="row">
        <div className="structured-diff-omitted" role="cell">
          Unchanged lines omitted
        </div>
      </div>
    );
  }

  return (
    <div
      className={`structured-diff-row structured-diff-row-${row.kind}${commentAction ? " structured-diff-row-with-actions" : ""}`}
      role="row"
    >
      <DiffCell cell={row.left} side="left" kind={row.kind} />
      <DiffCell cell={row.right} side="right" kind={row.kind} />
      {commentAction ? (
        <div className="structured-diff-row-actions" role="cell">
          <button
            className="ghost-button structured-diff-comment-button"
            type="button"
            onClick={commentAction.onClick}
            disabled={commentAction.disabled}
            aria-label={commentAction.label}
            title={commentAction.label}
          >
            Comment
          </button>
        </div>
      ) : null}
    </div>
  );
}

function DiffCell({
  cell,
  kind,
  side,
}: {
  cell: DiffPreviewCell;
  kind: DiffPreviewRow["kind"];
  side: "left" | "right";
}) {
  const sign = diffCellSign(kind, side);
  const isEmpty = cell.lineNumber === null && cell.text.length === 0;

  return (
    <div
      className={`structured-diff-cell structured-diff-cell-${side} ${
        isEmpty ? "structured-diff-cell-empty" : ""
      }`}
      role="cell"
    >
      <span className="structured-diff-line-number" aria-hidden="true">
        {cell.lineNumber ?? ""}
      </span>
      <span className="structured-diff-line-sign" aria-hidden="true">
        {sign}
      </span>
      <span className="structured-diff-line-text">
        {renderDiffText(cell.text, cell.highlights)}
      </span>
    </div>
  );
}

function renderDiffText(text: string, highlights: DiffPreviewHighlight[]) {
  if (text.length === 0) {
    return <span className="structured-diff-line-placeholder">&nbsp;</span>;
  }

  if (highlights.length === 0) {
    return text;
  }

  const segments: ReactNode[] = [];
  let cursor = 0;

  for (const [index, range] of highlights.entries()) {
    if (range.start > cursor) {
      segments.push(
        <span key={`plain:${index}:${cursor}`}>{text.slice(cursor, range.start)}</span>,
      );
    }
    segments.push(
      <mark key={`mark:${index}:${range.start}`} className="structured-diff-inline-change">
        {text.slice(range.start, range.end)}
      </mark>,
    );
    cursor = range.end;
  }

  if (cursor < text.length) {
    segments.push(<span key={`tail:${cursor}`}>{text.slice(cursor)}</span>);
  }

  return segments;
}

function diffCellSign(kind: DiffPreviewRow["kind"], side: "left" | "right") {
  if (kind === "added") {
    return side === "right" ? "+" : "";
  }
  if (kind === "removed") {
    return side === "left" ? "-" : "";
  }
  if (kind === "changed") {
    return side === "left" ? "-" : "+";
  }
  return "";
}

function formatHunkLabel(hunk: DiffPreviewHunk) {
  if (hunk.header) {
    return hunk.header;
  }

  const oldRange = formatHunkRange(hunk.oldStart, hunk.oldCount);
  const newRange = formatHunkRange(hunk.newStart, hunk.newCount);
  if (!oldRange && !newRange) {
    return "Patch changes";
  }

  return `${oldRange ?? "-"} -> ${newRange ?? "+"}`;
}

function formatHunkRange(start: number | null, count: number | null) {
  if (start === null) {
    return null;
  }
  if (count === null) {
    return `${start}`;
  }
  return `${start},${count}`;
}

function matchesLineThread(
  thread: ReviewThread,
  filePath: string,
  hunkHeader: string | null,
  row: DiffPreviewRow,
) {
  if (thread.anchor.kind !== "line" || hunkHeader === null) {
    return false;
  }

  if (thread.anchor.filePath !== filePath || thread.anchor.hunkHeader !== hunkHeader) {
    return false;
  }

  const oldMatches =
    thread.anchor.oldLine == null || thread.anchor.oldLine === row.left.lineNumber;
  const newMatches =
    thread.anchor.newLine == null || thread.anchor.newLine === row.right.lineNumber;

  return oldMatches && newMatches;
}

function ReviewThreadRow({ children }: { children: ReactNode }) {
  return (
    <div className="structured-diff-thread-row" role="row">
      <div className="structured-diff-thread-cell" role="cell">
        {children}
      </div>
    </div>
  );
}

function ReviewComposer({
  body,
  contextLabel,
  error,
  isSubmitting,
  mode,
  onBodyChange,
  onCancel,
  onSubmit,
}: {
  body: string;
  contextLabel: string;
  error: string | null;
  isSubmitting: boolean;
  mode: "newThread" | "reply";
  onBodyChange: (nextValue: string) => void;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  return (
    <div className="diff-review-composer">
      <label className="diff-review-composer-label">
        <span>{contextLabel}</span>
        <textarea
          value={body}
          onChange={(event) => onBodyChange(event.target.value)}
          rows={4}
          placeholder={mode === "reply" ? "Write a reply..." : "Write a review comment..."}
        />
      </label>
      {error ? <p className="support-copy diff-preview-note">{error}</p> : null}
      <div className="diff-review-composer-actions">
        <button
          className="ghost-button diff-review-action-button"
          type="button"
          onClick={onCancel}
          disabled={isSubmitting}
        >
          Cancel
        </button>
        <button
          className="ghost-button diff-review-action-button"
          type="button"
          onClick={onSubmit}
          disabled={isSubmitting}
        >
          {isSubmitting ? "Saving..." : mode === "reply" ? "Reply" : "Start thread"}
        </button>
      </div>
    </div>
  );
}

type ReviewComposerState =
  | {
      mode: "newThread";
      key: string;
      anchor: ReviewAnchor;
    }
  | {
      mode: "reply";
      key: string;
      threadId: string;
    };

type ReviewActionError = {
  key: string;
  message: string;
};

function createLineAnchor(
  filePath: string,
  hunkHeader: string,
  row: DiffPreviewRow,
): Extract<ReviewAnchor, { kind: "line" }> | null {
  if (row.kind === "omitted") {
    return null;
  }

  const oldLine = row.left.lineNumber;
  const newLine = row.right.lineNumber;
  if (oldLine == null && newLine == null) {
    return null;
  }

  return {
    kind: "line",
    filePath,
    hunkHeader,
    ...(oldLine == null ? {} : { oldLine }),
    ...(newLine == null ? {} : { newLine }),
  };
}

function reviewAnchorKey(anchor: ReviewAnchor) {
  if (anchor.kind === "changeSet") {
    return "change-set";
  }

  if (anchor.kind === "file") {
    return `file:${anchor.filePath}`;
  }

  if (anchor.kind === "hunk") {
    return `hunk:${anchor.filePath}:${anchor.hunkHeader}`;
  }

  return `line:${anchor.filePath}:${anchor.hunkHeader}:${anchor.oldLine ?? "-"}:${anchor.newLine ?? "-"}`;
}

function formatLineAnchorLabel(anchor: Extract<ReviewAnchor, { kind: "line" }>) {
  if (anchor.oldLine != null && anchor.newLine != null && anchor.oldLine !== anchor.newLine) {
    return `lines ${anchor.oldLine}/${anchor.newLine}`;
  }

  return `line ${anchor.newLine ?? anchor.oldLine ?? "?"}`;
}

function getActionErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The review action failed.";
}
