import { useEffect, useState } from "react";

import { fetchOrchestratorTemplates } from "../api";
import { sanitizeUserFacingErrorMessage } from "../error-messages";
import { ORCHESTRATOR_TEMPLATES_CHANGED_EVENT } from "../orchestrator-templates-events";
import type { OrchestratorTemplate } from "../types";

export function OrchestratorTemplateLibraryPanel({
  onNewCanvas,
  onOpenCanvas,
}: {
  onNewCanvas: () => void;
  onOpenCanvas: (templateId: string) => void;
}) {
  const [templates, setTemplates] = useState<OrchestratorTemplate[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function loadTemplates() {
      setIsLoading(true);
      setErrorMessage(null);
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
        setErrorMessage(getErrorMessage(error));
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

  return (
    <section className="control-panel-section-stack orchestrator-library-panel" aria-label="Orchestrators">
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

      {errorMessage ? (
        <p className="session-control-hint error">{errorMessage}</p>
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

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return sanitizeUserFacingErrorMessage(error.message);
  }

  return "Could not load orchestrator templates.";
}
