import { useEffect, useMemo, useRef, useState } from "react";

import {
  fetchOrchestratorTemplates,
  pauseOrchestratorInstance,
  resumeOrchestratorInstance,
  stopOrchestratorInstance,
  type StateResponse,
} from "../api";
import { sanitizeUserFacingErrorMessage } from "../error-messages";
import { ORCHESTRATOR_TEMPLATES_CHANGED_EVENT } from "../orchestrator-templates-events";
import type { OrchestratorInstance, OrchestratorTemplate } from "../types";

type OrchestratorAction = "pause" | "resume" | "stop";

export function OrchestratorTemplateLibraryPanel({
  onNewCanvas,
  onOpenCanvas,
  orchestrators = [],
  onStateUpdated,
}: {
  onNewCanvas: () => void;
  onOpenCanvas: (templateId: string) => void;
  orchestrators?: OrchestratorInstance[];
  onStateUpdated?: (state: StateResponse) => void;
}) {
  const [templates, setTemplates] = useState<OrchestratorTemplate[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [templateErrorMessage, setTemplateErrorMessage] = useState<string | null>(null);
  const [actionErrorMessage, setActionErrorMessage] = useState<string | null>(null);
  const [pendingActionByInstanceId, setPendingActionByInstanceId] = useState<
    Record<string, OrchestratorAction | undefined>
  >({});
  const isMountedRef = useRef(true);
  const runtimeInstances = useMemo(
    () => [...orchestrators].sort((left, right) => right.createdAt.localeCompare(left.createdAt)),
    [orchestrators],
  );

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;

    async function loadTemplates() {
      setIsLoading(true);
      setTemplateErrorMessage(null);
      try {
        const response = await fetchOrchestratorTemplates();
        if (cancelled) {
          return;
        }
        setTemplates(response.templates);
      } catch (error) {
        if (cancelled) {
          return;
        }
        setTemplateErrorMessage(getErrorMessage(error));
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    }

    void loadTemplates();
    const handleTemplatesChanged = () => {
      void loadTemplates();
    };
    window.addEventListener(ORCHESTRATOR_TEMPLATES_CHANGED_EVENT, handleTemplatesChanged);
    return () => {
      cancelled = true;
      window.removeEventListener(ORCHESTRATOR_TEMPLATES_CHANGED_EVENT, handleTemplatesChanged);
    };
  }, []);

  async function handleRuntimeAction(instanceId: string, action: OrchestratorAction) {
    setActionErrorMessage(null);
    setPendingActionByInstanceId((current) => ({
      ...current,
      [instanceId]: action,
    }));

    try {
      let state: StateResponse;
      switch (action) {
        case "pause":
          state = await pauseOrchestratorInstance(instanceId);
          break;
        case "resume":
          state = await resumeOrchestratorInstance(instanceId);
          break;
        case "stop":
          state = await stopOrchestratorInstance(instanceId);
          break;
      }

      if (!isMountedRef.current) {
        return;
      }
      onStateUpdated?.(state);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      setActionErrorMessage(getErrorMessage(error));
    } finally {
      if (isMountedRef.current) {
        setPendingActionByInstanceId((current) => ({
          ...current,
          [instanceId]: undefined,
        }));
      }
    }
  }

  return (
    <section className="control-panel-section-stack orchestrator-library-panel" aria-label="Orchestrators">
      <div className="orchestrator-library-runtime panel">
        <div className="orchestrator-library-intro">
          <div>
            <p className="session-control-label">Runtime instances</p>
            <p className="settings-panel-copy">
              Monitor live orchestration runs and pause, resume, or stop them without polling.
            </p>
          </div>
        </div>

        {actionErrorMessage ? (
          <p className="session-control-hint error">{actionErrorMessage}</p>
        ) : null}

        {runtimeInstances.length === 0 ? (
          <p className="session-control-hint">No orchestration runs are active right now.</p>
        ) : (
          <div className="orchestrator-library-list" role="list">
            {runtimeInstances.map((instance) => {
              const pendingAction = pendingActionByInstanceId[instance.id];
              const hasPendingAction = Boolean(pendingAction);
              return (
                <article key={instance.id} className="orchestrator-library-item panel" role="listitem">
                  <div className="orchestrator-library-item-copy">
                    <div className="card-label">{describeStatus(instance.status)}</div>
                    <h3>{instance.templateSnapshot.name}</h3>
                    <p>{describeRuntimeInstance(instance)}</p>
                  </div>
                  <dl className="orchestrator-library-item-meta">
                    <div>
                      <dt>Sessions</dt>
                      <dd>{instance.sessionInstances.length}</dd>
                    </div>
                    <div>
                      <dt>Pending</dt>
                      <dd>{instance.pendingTransitions?.length ?? 0}</dd>
                    </div>
                    <div>
                      <dt>Created</dt>
                      <dd>{instance.createdAt}</dd>
                    </div>
                    {instance.completedAt ? (
                      <div>
                        <dt>Completed</dt>
                        <dd>{instance.completedAt}</dd>
                      </div>
                    ) : null}
                  </dl>
                  {instance.errorMessage ? (
                    <p className="session-control-hint error">{instance.errorMessage}</p>
                  ) : null}
                  <div className="orchestrator-library-actions">
                    {instance.status === "running" ? (
                      <>
                        <button
                          className="ghost-button"
                          type="button"
                          onClick={() => handleRuntimeAction(instance.id, "pause")}
                          disabled={hasPendingAction}
                        >
                          {pendingAction === "pause" ? "Pausing..." : "Pause"}
                        </button>
                        <button
                          className="ghost-button"
                          type="button"
                          onClick={() => handleRuntimeAction(instance.id, "stop")}
                          disabled={hasPendingAction}
                        >
                          {pendingAction === "stop" ? "Stopping..." : "Stop"}
                        </button>
                      </>
                    ) : null}
                    {instance.status === "paused" ? (
                      <>
                        <button
                          className="ghost-button"
                          type="button"
                          onClick={() => handleRuntimeAction(instance.id, "resume")}
                          disabled={hasPendingAction}
                        >
                          {pendingAction === "resume" ? "Resuming..." : "Resume"}
                        </button>
                        <button
                          className="ghost-button"
                          type="button"
                          onClick={() => handleRuntimeAction(instance.id, "stop")}
                          disabled={hasPendingAction}
                        >
                          {pendingAction === "stop" ? "Stopping..." : "Stop"}
                        </button>
                      </>
                    ) : null}
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </div>

      <div className="orchestrator-library-intro">
        <div>
          <p className="session-control-label">Template library</p>
          <p className="settings-panel-copy">
            Browse saved orchestration templates, or open a blank canvas to design a new one.
          </p>
        </div>
        <button className="ghost-button" type="button" onClick={onNewCanvas}>
          New canvas
        </button>
      </div>

      {templateErrorMessage ? (
        <p className="session-control-hint error">{templateErrorMessage}</p>
      ) : isLoading ? (
        <p className="session-control-hint">Loading orchestration templates...</p>
      ) : templates.length === 0 ? (
        <div className="orchestrator-library-empty panel">
          <p className="session-control-hint">
            No orchestration templates yet. Start with a blank canvas and save your first flow.
          </p>
          <button className="send-button" type="button" onClick={onNewCanvas}>
            Create template canvas
          </button>
        </div>
      ) : (
        <div className="orchestrator-library-list" role="list">
          {templates.map((template) => (
            <article key={template.id} className="orchestrator-library-item panel" role="listitem">
              <div className="orchestrator-library-item-copy">
                <div className="card-label">Template</div>
                <h3>{template.name}</h3>
                {template.description.trim() ? <p>{template.description}</p> : null}
              </div>
              <dl className="orchestrator-library-item-meta">
                <div>
                  <dt>Sessions</dt>
                  <dd>{template.sessions.length}</dd>
                </div>
                <div>
                  <dt>Transitions</dt>
                  <dd>{template.transitions.length}</dd>
                </div>
                <div>
                  <dt>Updated</dt>
                  <dd>{template.updatedAt}</dd>
                </div>
              </dl>
              <button
                className="control-panel-header-action control-panel-header-open-button"
                type="button"
                onClick={() => onOpenCanvas(template.id)}
              >
                Edit canvas
              </button>
            </article>
          ))}
        </div>
      )}
    </section>
  );
}

function describeRuntimeInstance(instance: OrchestratorInstance) {
  const projectId = instance.projectId.trim();
  const context = projectId ? `Project ${projectId}` : "Projectless runtime";
  return `${context}. ${instance.sessionInstances.length} session${instance.sessionInstances.length === 1 ? "" : "s"} in this run.`;
}

function describeStatus(status: OrchestratorInstance["status"]) {
  switch (status) {
    case "running":
      return "Running";
    case "paused":
      return "Paused";
    case "stopped":
      return "Stopped";
  }
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return sanitizeUserFacingErrorMessage(error.message);
  }

  return "Could not load orchestrator templates.";
}