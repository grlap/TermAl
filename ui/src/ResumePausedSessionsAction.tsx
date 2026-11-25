/*
 * Owns the operator's "Resume paused sessions" action: a button that opens a
 * preview of every paused session on this host with its pause reason and
 * whether the action resumes or skips it, and a confirm step that runs the
 * action and shows each session's outcome. The dialog is a modal rendered on
 * the page body, so a long list scrolls inside it with its controls in view.
 * Does not resume anything itself or decide what is skipped: the host's
 * resume-paused endpoint does both, through each session's own Resume.
 * New module; nothing was moved here from another file.
 */
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";

import {
  resumePausedQueues as requestResumePausedQueues,
  type QueuePauseReason,
  type ResumePausedQueueRow,
  type ResumePausedQueuesResponse,
} from "./api";
import { isDialogBackdropDismissMouseDown } from "./dialog-backdrop-dismiss";
import { useDialogEscapeDismiss } from "./dialog-escape-dismiss";
import "./ResumePausedSessionsAction.css";

type Props = {
  /** Injected in tests; the host's resume-paused endpoint otherwise. */
  resumePausedQueues?: (
    dryRun: boolean,
    sessionIds?: string[],
  ) => Promise<ResumePausedQueuesResponse>;
};

type Phase =
  | { kind: "closed" }
  | { kind: "loading" }
  | { kind: "preview"; response: ResumePausedQueuesResponse }
  | { kind: "running"; response: ResumePausedQueuesResponse }
  | { kind: "done"; response: ResumePausedQueuesResponse }
  | { kind: "error"; message: string };

export function describePauseReason(reason: QueuePauseReason | null): string {
  if (!reason) {
    return "no recorded reason";
  }
  const cause = reason.cause.replace(/_/g, " ");
  const kind = reason.failureKind ? ` (${reason.failureKind.replace(/_/g, " ")})` : "";
  const summary = reason.summary ? `: ${reason.summary}` : "";
  return `${cause}${kind}${summary}`;
}

function describeAction(row: ResumePausedQueueRow, ran: boolean): string {
  if (row.action === "skip") {
    return `Skipped: ${(row.skipReason ?? "skipped").replace(/_/g, " ")}`;
  }
  if (!ran) {
    return "Will resume";
  }
  switch (row.outcome) {
    case "failed":
      return `Failed: ${row.error ?? "unknown error"}`;
    case "stillPaused":
      return "Resume accepted; still paused";
    case "notStarted":
      return "Not started: the run's time budget ran out; run it again";
    default:
      return "Resumed";
  }
}

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Tab and Shift+Tab cycle through the dialog's own controls only. */
function keepTabFocusInside(event: KeyboardEvent<HTMLElement>) {
  if (event.key !== "Tab") {
    return;
  }
  const container = event.currentTarget;
  const focusable = Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE));
  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  const active = document.activeElement;
  if (!first || !last) {
    event.preventDefault();
    container.focus();
  } else if (event.shiftKey && (active === first || active === container)) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && active === last) {
    event.preventDefault();
    first.focus();
  }
}

export function ResumePausedSessionsAction({
  resumePausedQueues = requestResumePausedQueues,
}: Props) {
  const [phase, setPhase] = useState<Phase>({ kind: "closed" });
  // Each open or close starts a new attempt; a response that arrives for an
  // earlier one, after the dialog was closed or reopened, is dropped.
  const attempt = useRef(0);

  async function run(
    pending: Phase,
    call: () => Promise<ResumePausedQueuesResponse>,
    settled: (response: ResumePausedQueuesResponse) => Phase,
  ) {
    const current = ++attempt.current;
    setPhase(pending);
    let next: Phase;
    try {
      next = settled(await call());
    } catch (error) {
      next = { kind: "error", message: String(error) };
    }
    if (attempt.current === current) {
      setPhase(next);
    }
  }

  const openPreview = () =>
    run(
      { kind: "loading" },
      () => resumePausedQueues(true),
      (response) => ({ kind: "preview", response }),
    );

  // Only the sessions the preview showed as resumable may resume.
  const confirm = (preview: ResumePausedQueuesResponse) =>
    run(
      { kind: "running", response: preview },
      () =>
        resumePausedQueues(
          false,
          preview.sessions
            .filter((row) => row.action === "resume")
            .map((row) => row.sessionId),
        ),
      (response) => ({ kind: "done", response }),
    );

  const close = () => {
    attempt.current += 1;
    setPhase({ kind: "closed" });
  };
  const isOpen = phase.kind !== "closed";
  useDialogEscapeDismiss({ isOpen, onDismiss: close });

  // Focus moves into the dialog when it opens and back to the button that
  // opened it when it closes.
  const opener = useRef<HTMLButtonElement>(null);
  const dialog = useRef<HTMLElement>(null);
  useEffect(() => {
    if (!isOpen) {
      return;
    }
    dialog.current?.focus();
    const restore = opener.current;
    return () => restore?.focus();
  }, [isOpen]);

  // A control that held focus can disappear between phases, such as the
  // Resume button once confirmed; focus then returns to the dialog itself.
  useEffect(() => {
    const element = dialog.current;
    if (element && !element.contains(document.activeElement)) {
      element.focus();
    }
  }, [phase.kind]);

  const response =
    phase.kind === "preview" || phase.kind === "running" || phase.kind === "done"
      ? phase.response
      : null;
  const resumable = response?.sessions.filter((row) => row.action === "resume").length ?? 0;

  return (
    <>
      <button ref={opener} className="chip" type="button" onClick={openPreview}>
        Resume paused…
      </button>
      {isOpen
        ? createPortal(
            <div
              className="dialog-backdrop"
              onMouseDown={(event) => {
                if (isDialogBackdropDismissMouseDown(event.nativeEvent)) {
                  close();
                }
              }}
            >
              <section
                ref={dialog}
                tabIndex={-1}
                className="dialog-card panel resume-paused-dialog"
                role="dialog"
                aria-modal="true"
                aria-labelledby="resume-paused-dialog-title"
                onMouseDown={(event) => event.stopPropagation()}
                onKeyDown={keepTabFocusInside}
              >
                <h2 id="resume-paused-dialog-title">Resume paused sessions</h2>
                <div className="resume-paused-dialog-body">
                  {phase.kind === "loading" ? <p>Loading paused sessions…</p> : null}
                  {phase.kind === "error" ? <p role="alert">{phase.message}</p> : null}
                  {response ? (
                    <>
                      {response.sessions.length === 0 ? (
                        <p>No paused sessions on this host.</p>
                      ) : (
                        <ul aria-label="Paused sessions">
                          {response.sessions.map((row) => (
                            <li key={row.sessionId} data-session-id={row.sessionId}>
                              <strong>{row.name}</strong>{" "}
                              <span>{describePauseReason(row.queuePauseReason)}</span>{" "}
                              <span>{describeAction(row, phase.kind === "done")}</span>
                            </li>
                          ))}
                        </ul>
                      )}
                      {response.pausedDelegationSessionsNotShown > 0 ? (
                        <p>
                          {response.pausedDelegationSessionsNotShown} paused delegation
                          session(s) not shown; their parent sessions own them.
                        </p>
                      ) : null}
                    </>
                  ) : null}
                </div>
                <div className="resume-paused-dialog-actions">
                  {phase.kind === "preview" ? (
                    <button
                      type="button"
                      disabled={resumable === 0}
                      onClick={() => confirm(phase.response)}
                    >
                      Resume {resumable} session(s)
                    </button>
                  ) : null}
                  <button type="button" onClick={close}>
                    {phase.kind === "done" || phase.kind === "running" ? "Close" : "Cancel"}
                  </button>
                  {phase.kind === "running" ? (
                    <p>Resuming; closing this does not stop sessions already resumed.</p>
                  ) : null}
                </div>
              </section>
            </div>,
            document.body,
          )
        : null}
    </>
  );
}
