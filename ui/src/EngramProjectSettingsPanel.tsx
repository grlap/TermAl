// Shared per-project Engram editor. Repository declaration comes from
// `.engram-project`; this surface enables the base MCP/context tier and keeps
// premium turn-gated control as a separate explicit opt-in.

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
  if (project.engram?.enabled && project.engram.turnGatedControl) {
    return "Enabled · turn-gated control";
  }
  if (project.engram?.enabled) {
    return "Enabled · base";
  }
  return "Declared · ready to enable";
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
  const [turnGatedControl, setTurnGatedControl] = useState(
    project.engram?.turnGatedControl === true,
  );
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

  useEffect(() => {
    setTurnGatedControl(project.engram?.turnGatedControl === true);
    setVerification(null);
    setError(null);
  }, [project.id, project.engram?.turnGatedControl]);

  function invalidateVerification() {
    setVerification(null);
    setError(null);
  }

  function buildEnablePayload(): EngramProjectSettings {
    return {
      enabled: true,
      turnGatedControl,
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
      setError("Verify this Engram project successfully before saving.");
      return;
    }
    setIsSaving(true);
    setError(null);
    try {
      const state = await updateProjectEngramSettings(
        project.id,
        buildEnablePayload(),
      );
      const savedProject = state.projects?.find(
        (candidate) => candidate.id === project.id,
      );
      if (savedProject) {
        setTurnGatedControl(savedProject.engram?.turnGatedControl === true);
      }
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
      const savedProject = state.projects?.find(
        (candidate) => candidate.id === project.id,
      );
      if (savedProject) {
        setTurnGatedControl(savedProject.engram?.turnGatedControl === true);
      }
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
          <div className="card-label">Repository-declared Engram</div>
          <h3>{project.name}</h3>
          <p className="settings-panel-copy">
            <code>.engram-project</code> declares this repository. Base mode
            injects Engram MCP and fresh work context into local sessions.
          </p>
        </div>
        <span className="remote-settings-badge">
          {describeProjectEngramState(project, currentVerificationState)}
        </span>
      </div>

      <label
        className="remote-settings-toggle"
        htmlFor={`${idPrefix}-turn-gated-control`}
      >
        <input
          id={`${idPrefix}-turn-gated-control`}
          aria-label="Turn-gated control"
          type="checkbox"
          checked={turnGatedControl}
          disabled={busy}
          onChange={(event) => {
            setTurnGatedControl(event.target.checked);
            invalidateVerification();
          }}
        />
        <span>
          <strong>Turn-gated control</strong>
          <small>
            Premium opt-in. Engram may evaluate and withhold turns. Base MCP
            and context stay available when this is off.
          </small>
        </span>
      </label>

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
        Disable is the global per-project kill switch for both tiers.
        Re-enabling requires a successful Verify followed by Save &amp; enable.
      </p>
    </div>
  );
}
