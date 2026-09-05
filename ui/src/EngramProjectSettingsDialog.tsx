// Project-scoped Engram settings dialog opened from a project context menu.
// The editor itself is shared with the global Settings > Engram panel.

import { useState } from "react";

import type { StateResponse } from "./api";
import { isDialogBackdropDismissMouseDown } from "./dialog-backdrop-dismiss";
import { useDialogEscapeDismiss } from "./dialog-escape-dismiss";
import {
  EngramProjectSettingsPanel,
  type ProjectEngramVerificationState,
} from "./EngramProjectSettingsPanel";
import { DialogCloseIcon } from "./message-card-icons";
import type { Project } from "./types";

export function EngramProjectSettingsDialog({
  project,
  onClose,
  onSaved,
  onVerified,
}: {
  project: Project;
  onClose: () => void;
  onSaved: (state: StateResponse) => void;
  onVerified: (
    projectId: string,
    state: ProjectEngramVerificationState,
  ) => void;
}) {
  const [busy, setBusy] = useState(false);

  useDialogEscapeDismiss({
    isOpen: true,
    onDismiss: busy ? () => undefined : onClose,
  });

  return (
    <div
      className="dialog-backdrop"
      onMouseDown={(event) => {
        if (!busy && isDialogBackdropDismissMouseDown(event.nativeEvent)) {
          onClose();
        }
      }}
    >
      <section
        className="dialog-card panel settings-dialog project-settings-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="project-settings-dialog-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="settings-dialog-header">
          <div>
            <div className="card-label">Project settings</div>
            <h2 id="project-settings-dialog-title">{project.name}</h2>
            <p className="dialog-copy settings-dialog-copy">
              Configure the base MCP/context tier and optional turn-gated
              control for this repository.
            </p>
          </div>
          <button
            className="ghost-button settings-dialog-close"
            type="button"
            aria-label="Close dialog"
            title="Close"
            disabled={busy}
            onClick={onClose}
          >
            <DialogCloseIcon />
          </button>
        </div>

        <div className="settings-dialog-body">
          <div className="settings-dialog-content">
            <div
              className="settings-tab-list"
              role="tablist"
              aria-label="Project settings sections"
              aria-orientation="vertical"
            >
              <button
                id="project-settings-tab-engram"
                className="settings-tab selected"
                type="button"
                role="tab"
                aria-selected="true"
                aria-controls="project-settings-panel-engram"
              >
                Engram
              </button>
            </div>

            <div
              id="project-settings-panel-engram"
              role="tabpanel"
              aria-labelledby="project-settings-tab-engram"
            >
              <EngramProjectSettingsPanel
                project={project}
                idPrefix="project-engram"
                onBusyChange={setBusy}
                onCancel={onClose}
                onSaved={(state) => {
                  onSaved(state);
                  onClose();
                }}
                onVerified={onVerified}
              />
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}
