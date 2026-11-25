import { useEffect, useId, useRef, useState } from "react";
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
};

type AccountResponse = {
  status: AccountStatus;
  challenge?: { attemptId: string; authorizationUrl: string };
};

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
  const [challenge, setChallenge] = useState<AccountResponse["challenge"]>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

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
          if (controller.signal.aborted) return;
          setStatus(result.status);
          setError(null);
          if (result.status.phase !== "pending") setChallenge(undefined);
        })
        .catch((cause: unknown) => {
          if (!controller.signal.aborted) {
            setError(cause instanceof Error ? cause.message : "Could not read Codex sign-in status.");
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
    setBusy(true);
    setError(null);
    try {
      const result = await accountRequest(action);
      setStatus(result.status);
      setChallenge(result.status.phase === "pending" ? result.challenge : undefined);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "The Codex account action failed.");
      if (action.action === "cancel" && cause instanceof ApiRequestError && cause.status === 409) {
        setChallenge(undefined);
        setStatus(null);
      }
    } finally {
      setBusy(false);
    }
  }

  function dismiss() {
    onClose();
  }

  const pending = status?.phase === "pending";
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
      {error && <p role="alert">{error}</p>}
      {challenge && (
        <p>
          <a href={challenge.authorizationUrl} target="_blank" rel="noreferrer">
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
          <button type="button" disabled={busy} onClick={() => void perform({ action: initialAction })}>
            {primaryLabel}
          </button>
        )}
        {challenge && (
          <button type="button" disabled={busy}
            onClick={() => void perform({ action: "cancel", attemptId: challenge.attemptId })}>
            Cancel sign-in
          </button>
        )}
        <button type="button" onClick={dismiss}>Close</button>
      </div>
    </dialog>
  );
}
