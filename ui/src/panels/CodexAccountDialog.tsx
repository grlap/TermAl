import { useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import { ApiRequestError, request } from "../api-request";
import type { AgentType } from "../types";

export function codexAccountCommand(
  agent: AgentType | undefined,
  draft: string,
): "login" | "logout" | null {
  if (agent !== "Codex") return null;
  const command = draft.trim();
  if (command === "/login") return "login";
  if (command === "/logout") return "logout";
  return null;
}

type AccountAction =
  | { action: "status" | "login" | "logout" | "useStandalone" }
  | { action: "cancel"; attemptId: string };

type AccountStatus = {
  mode: "standalone" | "enrolling" | "managed";
  phase: "standalone" | "pending" | "needsLogin" | "signedIn" | "signedOut" | "unknown";
  message: string;
  canUseStandalone: boolean;
  // Informational only: a managed Codex runtime reported Unauthorized.
  notice?: string;
};

type AccountResponse = {
  status: AccountStatus;
  challenge?: { attemptId: string; authorizationUrl: string };
};

// The status endpoint never returns a challenge. Keep the initiating tab's
// copy in memory across dialog and session panel lifetimes.
let accountAction = { busy: false, challenge: undefined as AccountResponse["challenge"] };
const accountActionListeners = new Set<() => void>();

function subscribeToAccountAction(listener: () => void) {
  accountActionListeners.add(listener);
  return () => { accountActionListeners.delete(listener); };
}

function getAccountAction() {
  return accountAction;
}

function updateAccountAction(update: Partial<typeof accountAction>) {
  accountAction = { ...accountAction, ...update };
  for (const listener of accountActionListeners) listener();
}

function accountRequest(body: AccountAction, signal?: AbortSignal) {
  return request<AccountResponse>("/api/codex/account", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    cache: "no-store",
    signal,
  });
}

export function CodexAccountDialog({
  initialAction,
  onClose,
}: {
  initialAction: "login" | "logout";
  onClose: () => void;
}) {
  const headingId = useId();
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [status, setStatus] = useState<AccountStatus | null>(null);
  const { busy, challenge } = useSyncExternalStore(subscribeToAccountAction, getAccountAction);
  const [actionError, setActionError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);

  useEffect(() => {
    if (busy || (status && status.phase !== "pending")) return;
    const controller = new AbortController();
    const readStatus = () => {
      void accountRequest({ action: "status" }, controller.signal)
        .then((result) => {
          if (controller.signal.aborted || getAccountAction().busy) return;
          setStatus(result.status);
          setStatusError(null);
          if (result.status.phase !== "pending" && getAccountAction().challenge) {
            updateAccountAction({ challenge: undefined });
          }
        })
        .catch((cause: unknown) => {
          if (!controller.signal.aborted) {
            setStatusError(cause instanceof Error ? cause.message : "Could not read Codex sign-in status.");
            timer = window.setTimeout(readStatus, 800);
          }
        });
    };
    let timer = window.setTimeout(readStatus, status ? 800 : 0);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [busy, status]);

  async function perform(action: AccountAction) {
    if (getAccountAction().busy) return;
    updateAccountAction({ busy: true });
    setActionError(null);
    setStatusError(null);
    try {
      const result = await accountRequest(action);
      setStatus(result.status);
      updateAccountAction({ challenge: result.status.phase === "pending" ? result.challenge : undefined });
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "The Codex account action failed.");
      setStatus(null);
      if (action.action === "cancel" && cause instanceof ApiRequestError && cause.status === 409) {
        updateAccountAction({ challenge: undefined });
      }
    } finally {
      updateAccountAction({ busy: false });
    }
  }

  function dismiss() {
    onClose();
  }

  const pending = status?.phase === "pending";
  const liveChallenge = pending ? challenge : undefined;
  const error = actionError ?? statusError;
  const primaryLabel = initialAction === "login" ? "Sign in with ChatGPT" : "Sign out";

  return (
    <dialog
      className="dialog-card panel"
      ref={dialogRef}
      aria-labelledby={headingId}
      onCancel={(event) => {
        event.preventDefault();
        dismiss();
      }}
    >
      <h2 id={headingId}>Codex account</h2>
      <p className="dialog-copy">
        This changes Codex sign-in for TermAl on {window.location.host}. After
        you sign in here, standalone Codex sign-in no longer changes TermAl’s
        account. Future sends can resume saved sessions under the selected account.
      </p>
      <p role="status">{status?.message ?? "Checking Codex sign-in…"}</p>
      {status?.notice && <p className="dialog-copy">{status.notice}</p>}
      {error && <p role="alert">{error}</p>}
      {liveChallenge && (
        <p>
          <a href={liveChallenge.authorizationUrl} target="_blank" rel="noreferrer">
            Continue sign-in in your browser
          </a>
        </p>
      )}
      <div className="dialog-actions">
        {status?.canUseStandalone && (
          <button type="button" disabled={busy} onClick={() => void perform({ action: "useStandalone" })}>
            Use standalone Codex sign-in
          </button>
        )}
        {!pending && (
          <button type="button" disabled={busy || !status} onClick={() => void perform({ action: initialAction })}>
            {primaryLabel}
          </button>
        )}
        {liveChallenge && (
          <button type="button" disabled={busy}
            onClick={() => void perform({ action: "cancel", attemptId: liveChallenge.attemptId })}>
            Cancel sign-in
          </button>
        )}
        <button type="button" onClick={dismiss}>Close</button>
      </div>
    </dialog>
  );
}
