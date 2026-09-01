// Global Settings > Engram surface. Host binary/home are machine-scoped;
// repositories appear automatically when their root contains `.engram-project`.

import { useEffect, useMemo, useState } from "react";

import { updateEngramHostSettings, type StateResponse } from "../api";
import {
  describeProjectEngramState,
  EngramProjectSettingsPanel,
  type ProjectEngramVerificationState,
} from "../EngramProjectSettingsPanel";
import { isLocalRemoteId } from "../remotes";
import type { EngramHostSettings, Project } from "../types";

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function EngramPreferencesPanel({
  hostSettings,
  projects,
  onStateUpdated,
}: {
  hostSettings: EngramHostSettings;
  projects: Project[];
  onStateUpdated: (state: StateResponse) => void;
}) {
  const declaredProjects = useMemo(
    () =>
      projects.filter(
        (project) =>
          isLocalRemoteId(project.remoteId) && project.engramDeclared,
      ),
    [projects],
  );
  const [selectedProjectId, setSelectedProjectId] = useState(
    () => declaredProjects[0]?.id ?? "",
  );
  const [binaryPath, setBinaryPath] = useState(hostSettings.binaryPath);
  const [home, setHome] = useState(hostSettings.home);
  const [bootRecoveryBudgetMs, setBootRecoveryBudgetMs] = useState(
    hostSettings.bootRecoveryBudgetMs,
  );
  const [isSavingHost, setIsSavingHost] = useState(false);
  const [hostNotice, setHostNotice] = useState<string | null>(null);
  const [hostError, setHostError] = useState<string | null>(null);
  const [verificationByProjectId, setVerificationByProjectId] = useState<
    Record<string, ProjectEngramVerificationState>
  >({});

  useEffect(() => {
    setBinaryPath(hostSettings.binaryPath);
    setHome(hostSettings.home);
    setBootRecoveryBudgetMs(hostSettings.bootRecoveryBudgetMs);
  }, [
    hostSettings.binaryPath,
    hostSettings.bootRecoveryBudgetMs,
    hostSettings.home,
  ]);

  useEffect(() => {
    if (declaredProjects.some((project) => project.id === selectedProjectId)) {
      return;
    }
    setSelectedProjectId(declaredProjects[0]?.id ?? "");
  }, [declaredProjects, selectedProjectId]);

  const selectedProject =
    declaredProjects.find((project) => project.id === selectedProjectId) ??
    null;
  const verification = selectedProject
    ? verificationByProjectId[selectedProject.id]
    : undefined;

  async function handleSaveHost() {
    setIsSavingHost(true);
    setHostNotice(null);
    setHostError(null);
    try {
      const state = await updateEngramHostSettings({
        binaryPath: binaryPath.trim() || "engram",
        home: home.trim(),
        bootRecoveryBudgetMs,
      });
      onStateUpdated(state);
      setHostNotice("Host Engram settings saved.");
    } catch (error) {
      setHostError(errorMessage(error));
    } finally {
      setIsSavingHost(false);
    }
  }

  return (
    <section className="settings-panel-stack engram-preferences-panel">
      <article className="message-card prompt-settings-card engram-host-settings-card">
        <div className="settings-section-header">
          <div>
            <div className="card-label">Host-global</div>
            <h3>Engram runtime</h3>
            <p className="settings-panel-copy">
              Configure the executable and Engram home once for this machine.
              Projects never override these paths.
            </p>
          </div>
          <span className="remote-settings-badge">This machine</span>
        </div>

        <div className="project-engram-form">
          <label
            className="create-session-field"
            htmlFor="settings-engram-host-binary"
          >
            <span>Binary path</span>
            <input
              id="settings-engram-host-binary"
              aria-label="Engram host binary path"
              className="themed-input"
              value={binaryPath}
              disabled={isSavingHost}
              onChange={(event) => {
                setBinaryPath(event.target.value);
                setHostNotice(null);
                setHostError(null);
              }}
            />
            <span className="create-session-field-hint">
              Defaults to <code>engram</code> resolved on PATH.
            </span>
          </label>
          <label
            className="create-session-field"
            htmlFor="settings-engram-host-home"
          >
            <span>Home</span>
            <input
              id="settings-engram-host-home"
              aria-label="Engram host home"
              className="themed-input"
              value={home}
              disabled={isSavingHost}
              onChange={(event) => {
                setHome(event.target.value);
                setHostNotice(null);
                setHostError(null);
              }}
            />
            <span className="create-session-field-hint">
              Defaults to the server user's <code>.engram</code> directory.
            </span>
          </label>
          <label
            className="create-session-field"
            htmlFor="settings-engram-boot-recovery-budget"
          >
            <span>Boot recovery budget (ms)</span>
            <input
              id="settings-engram-boot-recovery-budget"
              aria-label="Engram boot recovery budget"
              className="themed-input"
              type="number"
              min={100}
              max={60_000}
              step={100}
              value={bootRecoveryBudgetMs}
              disabled={isSavingHost}
              onChange={(event) => {
                setBootRecoveryBudgetMs(event.currentTarget.valueAsNumber);
                setHostNotice(null);
                setHostError(null);
              }}
            />
            <span className="create-session-field-hint">
              Eager recovery stops at this wall-clock budget; unfinished
              sessions retry when next used.
            </span>
          </label>
        </div>

        {hostNotice ? (
          <p className="session-control-hint" role="status">
            {hostNotice}
          </p>
        ) : null}
        {hostError ? (
          <p className="inline-error" role="alert">
            {hostError}
          </p>
        ) : null}
        <div className="dialog-actions engram-host-actions">
          <button
            className="primary-button engram-host-save-button"
            type="button"
            disabled={isSavingHost}
            onClick={() => void handleSaveHost()}
          >
            {isSavingHost ? "Saving…" : "Save host settings"}
          </button>
        </div>
      </article>

      <article className="message-card prompt-settings-card engram-project-picker-card">
        <div className="settings-section-header">
          <div>
            <div className="card-label">Repository declarations</div>
            <h3>Authorized projects</h3>
            <p className="settings-panel-copy">
              A local project appears here automatically when its repository
              contains a non-empty <code>.engram-project</code> file.
            </p>
          </div>
          <span className="remote-settings-badge">
            {declaredProjects.length} of {projects.length} declared
          </span>
        </div>

        {declaredProjects.length ? (
          <label
            className="create-session-field engram-project-picker"
            htmlFor="settings-engram-project"
          >
            <span>Project</span>
            <select
              id="settings-engram-project"
              aria-label="Engram project"
              className="themed-input"
              value={selectedProjectId}
              onChange={(event) => setSelectedProjectId(event.target.value)}
            >
              {declaredProjects.map((project) => (
                <option key={project.id} value={project.id}>
                  {project.name} —{" "}
                  {describeProjectEngramState(
                    project,
                    verificationByProjectId[project.id],
                  )}
                </option>
              ))}
            </select>
            <span className="create-session-field-hint">
              One write-only grant is required for each declared repository.
            </span>
          </label>
        ) : (
          <p className="settings-panel-copy" role="status">
            No local project declares Engram yet. Add a tracked
            <code> .engram-project</code> file to a repository and refresh the
            state.
          </p>
        )}
      </article>

      {selectedProject ? (
        <article className="message-card prompt-settings-card engram-settings-card">
          <EngramProjectSettingsPanel
            key={selectedProject.id}
            project={selectedProject}
            idPrefix="settings-engram"
            onSaved={onStateUpdated}
            onVerified={(projectId, nextVerification) =>
              setVerificationByProjectId((current) => ({
                ...current,
                [projectId]: nextVerification,
              }))
            }
          />
          {verification ? (
            <p className="session-control-hint" role="status">
              {verification.detail}
            </p>
          ) : null}
        </article>
      ) : null}
    </section>
  );
}
