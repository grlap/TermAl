import { useEffect, useMemo, useRef, useState } from "react";

import {
  createOrchestratorInstance,
  fetchOrchestratorTemplates,
  pauseOrchestratorInstance,
  resumeOrchestratorInstance,
  stopOrchestratorInstance,
  type StateResponse,
} from "../api";
import { sanitizeUserFacingErrorMessage } from "../error-messages";
import { ORCHESTRATOR_TEMPLATES_CHANGED_EVENT } from "../orchestrator-templates-events";
import {
  RuntimeActionButton,
  RuntimeActionIcon,
  type RuntimeAction,
} from "../runtime-action-button";
import type { OrchestratorInstance, OrchestratorTemplate } from "../types";

type OrchestratorAction = RuntimeAction;
type OrchestratorLibraryButtonAction =
  | OrchestratorAction
  | "run"
  | "edit"
  | "new";
type OrchestratorLibraryStandaloneAction = Exclude<
  OrchestratorLibraryButtonAction,
  OrchestratorAction
>;

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
  const [templateActionErrorMessage, setTemplateActionErrorMessage] = useState<string | null>(null);
  const [actionErrorMessage, setActionErrorMessage] = useState<string | null>(null);
  const [pendingActionByInstanceId, setPendingActionByInstanceId] = useState<
    Record<string, OrchestratorAction | undefined>
  >({});
  const [pendingTemplateRunById, setPendingTemplateRunById] = useState<
    Record<string, true | undefined>
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

  async function handleTemplateRun(template: OrchestratorTemplate) {
    setTemplateActionErrorMessage(null);
    setPendingTemplateRunById((current) => ({
      ...current,
      [template.id]: true,
    }));

    try {
      const response = await createOrchestratorInstance(
        template.id,
        template.projectId ?? null,
      );
      if (!isMountedRef.current) {
        return;
      }
      onStateUpdated?.(response.state);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      setTemplateActionErrorMessage(getErrorMessage(error));
    } finally {
      if (isMountedRef.current) {
        setPendingTemplateRunById((current) => ({
          ...current,
          [template.id]: undefined,
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
                  <div className="orchestrator-library-item-header">
                    <div className="orchestrator-library-item-copy">
                      <div className="card-label">{describeStatus(instance.status)}</div>
                      <h3>{instance.templateSnapshot.name}</h3>
                      <p>{describeRuntimeInstance(instance)}</p>
                    </div>
                    <div className="orchestrator-library-actions">
                      {instance.status === "running" ? (
                        <>
                          <OrchestratorLibraryActionButton
                            action="pause"
                            isPending={pendingAction === "pause"}
                            disabled={hasPendingAction}
                            onClick={() => handleRuntimeAction(instance.id, "pause")}
                          />
                          <OrchestratorLibraryActionButton
                            action="stop"
                            isPending={pendingAction === "stop"}
                            disabled={hasPendingAction}
                            onClick={() => handleRuntimeAction(instance.id, "stop")}
                          />
                        </>
                      ) : null}
                      {instance.status === "paused" ? (
                        <>
                          <OrchestratorLibraryActionButton
                            action="resume"
                            isPending={pendingAction === "resume"}
                            disabled={hasPendingAction}
                            onClick={() => handleRuntimeAction(instance.id, "resume")}
                          />
                          <OrchestratorLibraryActionButton
                            action="stop"
                            isPending={pendingAction === "stop"}
                            disabled={hasPendingAction}
                            onClick={() => handleRuntimeAction(instance.id, "stop")}
                          />
                        </>
                      ) : null}
                    </div>
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
        <div className="orchestrator-library-intro-actions">
          <OrchestratorLibraryActionButton
            action="new"
            onClick={onNewCanvas}
          />
        </div>
      </div>

      {templateActionErrorMessage ? (
        <p className="session-control-hint error">{templateActionErrorMessage}</p>
      ) : null}

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
              <div className="orchestrator-library-item-header">
                <div className="orchestrator-library-item-copy">
                  <div className="card-label">Template</div>
                  <h3>{template.name}</h3>
                  {template.description.trim() ? <p>{template.description}</p> : null}
                </div>
                <div className="orchestrator-library-actions">
                  <OrchestratorLibraryActionButton
                    action="run"
                    isPending={Boolean(pendingTemplateRunById[template.id])}
                    disabled={Boolean(pendingTemplateRunById[template.id])}
                    onClick={() => void handleTemplateRun(template)}
                  />
                  <OrchestratorLibraryActionButton
                    action="edit"
                    onClick={() => onOpenCanvas(template.id)}
                  />
                </div>
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

function OrchestratorLibraryActionButton({
  action,
  isPending,
  disabled,
  onClick,
}: {
  action: OrchestratorLibraryButtonAction;
  isPending?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  const label = describeLibraryActionLabel(action);
  const title = isPending
    ? describeLibraryActionPendingLabel(action)
    : label;

  if (isRuntimeAction(action)) {
    return (
      <RuntimeActionButton
        action={action}
        ariaLabel={label}
        title={title}
        classNamePrefix="orchestrator-library-action"
        isPending={isPending}
        disabled={disabled}
        onClick={onClick}
      />
    );
  }

  return (
    <button
      className={`ghost-button orchestrator-library-action orchestrator-library-action-${action}`}
      type="button"
      aria-label={label}
      title={title}
      aria-busy={isPending ? true : undefined}
      onClick={onClick}
      disabled={disabled}
    >
      {isPending ? (
        <span
          className="activity-spinner orchestrator-library-action-spinner"
          aria-hidden="true"
        />
      ) : (
        <OrchestratorLibraryActionIcon action={action} />
      )}
    </button>
  );
}

function OrchestratorLibraryActionIcon({
  action,
}: {
  action: OrchestratorLibraryStandaloneAction;
}) {
  if (action === "run") {
    return (
      <RuntimeActionIcon
        action="resume"
        classNamePrefix="orchestrator-library-action-icon"
      />
    );
  }

  if (action === "edit") {
    return (
      <svg
        className="orchestrator-library-action-icon"
        viewBox="0 0 16 16"
        focusable="false"
        aria-hidden="true"
      >
        <path
          d="M4.1 11.9 3.5 12.5l.45-2.25 6.6-6.6a1.5 1.5 0 0 1 2.12 2.12l-6.6 6.6L4.1 11.9Z"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="1.25"
        />
        <path
          d="M9.75 4.5 11.5 6.25"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeWidth="1.25"
        />
      </svg>
    );
  }

  if (action === "new") {
    return (
      <svg
        className="orchestrator-library-action-icon"
        viewBox="0 0 16 16"
        focusable="false"
        aria-hidden="true"
      >
        <rect
          x="2.75"
          y="3.25"
          width="8.5"
          height="6"
          rx="1"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.2"
        />
        <path
          d="M6 9.25v3.25M4.375 10.875h3.25"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeWidth="1.2"
        />
        <path
          d="M5 1.9v1.35M9 1.9v1.35"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeWidth="1.2"
        />
      </svg>
    );
  }

  return (
    <svg
      className="orchestrator-library-action-icon orchestrator-library-action-icon-stop"
      viewBox="0 0 16 16"
      focusable="false"
      aria-hidden="true"
    >
      <rect x="4.35" y="4.35" width="7.3" height="7.3" rx="1.2" fill="currentColor" />
    </svg>
  );
}

function isRuntimeAction(
  action: OrchestratorLibraryButtonAction,
): action is OrchestratorAction {
  return action === "pause" || action === "resume" || action === "stop";
}
function describeLibraryActionLabel(action: OrchestratorLibraryButtonAction) {
  switch (action) {
    case "pause":
      return "Pause";
    case "resume":
      return "Resume";
    case "stop":
      return "Stop";
    case "run":
      return "Run orchestration";
    case "edit":
      return "Edit canvas";
    case "new":
      return "New canvas";
  }
}

function describeLibraryActionPendingLabel(action: OrchestratorLibraryButtonAction) {
  switch (action) {
    case "pause":
      return "Pausing";
    case "resume":
      return "Resuming";
    case "stop":
      return "Stopping";
    case "run":
      return "Starting orchestration";
    case "edit":
      return "Opening canvas";
    case "new":
      return "Creating canvas";
  }
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return sanitizeUserFacingErrorMessage(error.message);
  }

  return "Could not load orchestrator templates.";
}
