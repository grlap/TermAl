// Shared per-project Engram authority editor. Repository declaration comes
// from `.engram-project`; this host surface only installs a write-only grant,
// verifies it, enables the adapter, or applies an operator veto.

import { useEffect, useState } from "react";

import {
  updateProjectEngramSettings,
  verifyProjectEngramSettings,
  type StateResponse,
} from "./api";
import type {
  EngramProjectSettings,
  EngramProjectVerification,
  Project,
} from "./types";

export type ProjectEngramVerificationState = {
  verified: boolean;
  checkedAt: number;
  detail: string;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function formatOptionalStatus(value: string | null | undefined): string {
  return value?.trim() || "—";
}

export function describeProjectEngramState(
  project: Project,
  verification?: ProjectEngramVerificationState,
): string {
  if (!project.engramDeclared) {
    return "Not declared";
  }
  if (verification && !verification.verified) {
    return `Verify failed · ${verification.detail}`;
  }
  if (project.engramOperatorDisabled) {
    return "Declared · operator vetoed";
  }
  if (project.engram?.enabled && project.engramGrantConfigured) {
    return "Enabled";
  }
  return "Declared · awaiting grant";
}

export function EngramProjectSettingsPanel({
  project,
  idPrefix,
  onCancel,
  onSaved,
  onVerified,
  onBusyChange,
}: {
  project: Project;
  idPrefix: string;
  onCancel?: () => void;
  onSaved: (state: StateResponse) => void;
  onVerified: (
    projectId: string,
    state: ProjectEngramVerificationState,
  ) => void;
  onBusyChange?: (busy: boolean) => void;
}) {
  const [grant, setGrant] = useState("");
  const [verification, setVerification] =
    useState<EngramProjectVerification | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isVerifying, setIsVerifying] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isDisabling, setIsDisabling] = useState(false);
  const busy = isVerifying || isSaving || isDisabling;

  useEffect(() => {
    onBusyChange?.(busy);
    return () => onBusyChange?.(false);
  }, [busy, onBusyChange]);

  function invalidateVerification() {
    setVerification(null);
    setError(null);
  }

  function buildEnablePayload(): EngramProjectSettings {
    const normalizedGrant = grant.trim();
    return {
      enabled: true,
      ...(normalizedGrant ? { workAuthorityGrant: normalizedGrant } : {}),
    };
  }

  async function handleVerify() {
    setIsVerifying(true);
    setError(null);
    try {
      const result = await verifyProjectEngramSettings(
        project.id,
        buildEnablePayload(),
      );
      setVerification(result);
      onVerified(project.id, {
        verified: result.verified,
        checkedAt: Date.now(),
        detail: result.verified
          ? `Verified · ${result.projectId}`
          : result.errors?.[0] || "Verification failed",
      });
    } catch (verifyError) {
      const detail = errorMessage(verifyError);
      setVerification(null);
      setError(detail);
      onVerified(project.id, {
        verified: false,
        checkedAt: Date.now(),
        detail,
      });
    } finally {
      setIsVerifying(false);
    }
  }

  async function handleSave() {
    if (!verification?.verified) {
      setError("Verify this grant successfully before saving.");
      return;
    }
    setIsSaving(true);
    setError(null);
    try {
      const state = await updateProjectEngramSettings(
        project.id,
        buildEnablePayload(),
      );
      setGrant("");
      onSaved(state);
      onVerified(project.id, {
        verified: true,
        checkedAt: Date.now(),
        detail: `Enabled · ${verification.projectId}`,
      });
    } catch (saveError) {
      setError(errorMessage(saveError));
    } finally {
      setIsSaving(false);
    }
  }

  async function handleDisable() {
    setIsDisabling(true);
    setError(null);
    try {
      const state = await updateProjectEngramSettings(project.id, {
        enabled: false,
      });
      setGrant("");
      setVerification(null);
      onSaved(state);
      onVerified(project.id, {
        verified: true,
        checkedAt: Date.now(),
        detail: "Operator vetoed",
      });
    } catch (disableError) {
      setError(errorMessage(disableError));
    } finally {
      setIsDisabling(false);
    }
  }

  const saveDisabled = busy || !verification?.verified;
  const canDisable = project.engram?.enabled === true;
  const currentVerificationState: ProjectEngramVerificationState | undefined =
    verification
      ? {
          verified: verification.verified,
          checkedAt: 0,
          detail: verification.verified
            ? `Verified · ${verification.projectId}`
            : verification.errors?.[0] || "Verification failed",
        }
      : undefined;

  return (
    <div className="settings-panel project-engram-settings-panel">
      <div className="settings-section-header">
        <div>
          <div className="card-label">Repository-declared control</div>
          <h3>{project.name}</h3>
          <p className="settings-panel-copy">
            <code>.engram-project</code> declares this repository. The host
            authorizes it with one write-only grant.
          </p>
        </div>
        <span className="remote-settings-badge">
          {describeProjectEngramState(project, currentVerificationState)}
        </span>
      </div>

      <div className="project-engram-grant-form">
        <label
          className="create-session-field"
          htmlFor={`${idPrefix}-work-authority-grant`}
        >
          <span>Work authority grant</span>
          <input
            id={`${idPrefix}-work-authority-grant`}
            aria-label="Work authority grant"
            className="themed-input"
            type="password"
            autoComplete="new-password"
            value={grant}
            disabled={busy}
            placeholder="Leave blank to verify the stored grant"
            onChange={(event) => {
              setGrant(event.target.value);
              invalidateVerification();
            }}
          />
          <span className="create-session-field-hint">
            Write-only secret. TermAl never renders the stored value back.
          </span>
        </label>
      </div>

      {verification ? (
        <section
          className={`project-engram-verification ${verification.verified ? "verified" : "failed"}`}
          aria-label="Engram verification result"
        >
          <div className="project-engram-verification-title">
            {verification.verified ? "Verified" : "Verification failed"}
          </div>
          <dl>
            <div>
              <dt>Project ID</dt>
              <dd>{formatOptionalStatus(verification.projectId)}</dd>
            </div>
            <div>
              <dt>Database</dt>
              <dd>{formatOptionalStatus(verification.database)}</dd>
            </div>
            <div>
              <dt>Host binary</dt>
              <dd>{formatOptionalStatus(verification.binaryPath)}</dd>
            </div>
            <div>
              <dt>Host home</dt>
              <dd>{formatOptionalStatus(verification.home)}</dd>
            </div>
            <div>
              <dt>Required assurance</dt>
              <dd>{formatOptionalStatus(verification.requiredAssurance)}</dd>
            </div>
            <div>
              <dt>Healthy</dt>
              <dd>{verification.healthy ? "Yes" : "No"}</dd>
            </div>
            <div>
              <dt>Grant installed</dt>
              <dd>
                {verification.grant.configured
                  ? verification.grant.installed
                    ? "Yes"
                    : "No"
                  : "Not configured"}
              </dd>
            </div>
            <div>
              <dt>Grant subject</dt>
              <dd>{formatOptionalStatus(verification.grant.subjectActorId)}</dd>
            </div>
            <div>
              <dt>Valid until</dt>
              <dd>{formatOptionalStatus(verification.grant.validUntil)}</dd>
            </div>
            <div>
              <dt>Revoked</dt>
              <dd>
                {verification.grant.revokedAt
                  ? `Yes · ${verification.grant.revokedAt}`
                  : "No"}
              </dd>
            </div>
          </dl>
          {verification.errors?.length ? (
            <ul className="project-engram-verification-errors">
              {verification.errors.map((message) => (
                <li key={message}>{message}</li>
              ))}
            </ul>
          ) : null}
        </section>
      ) : null}

      {project.engramCleanupWarning ? (
        <p className="inline-error" role="status">
          {project.engramCleanupWarning}
        </p>
      ) : null}
      {error ? (
        <p className="inline-error" role="alert">
          {error}
        </p>
      ) : null}

      <div className="dialog-actions project-engram-actions">
        {onCancel ? (
          <button
            className="ghost-button"
            type="button"
            disabled={busy}
            onClick={onCancel}
          >
            Cancel
          </button>
        ) : null}
        {canDisable ? (
          <button
            className="danger-button"
            type="button"
            disabled={busy}
            onClick={() => void handleDisable()}
          >
            {isDisabling ? "Disabling…" : "Disable Engram"}
          </button>
        ) : null}
        <button
          className="ghost-button"
          type="button"
          disabled={busy}
          onClick={() => void handleVerify()}
        >
          {isVerifying ? "Verifying…" : "Verify"}
        </button>
        <button
          className="primary-button"
          type="button"
          disabled={saveDisabled}
          onClick={() => void handleSave()}
        >
          {isSaving ? "Saving…" : "Save & enable"}
        </button>
      </div>
      <p className="create-session-field-hint">
        Disable is an unconditional host-side veto. Re-enabling requires a
        successful Verify followed by Save &amp; enable.
      </p>
    </div>
  );
}
