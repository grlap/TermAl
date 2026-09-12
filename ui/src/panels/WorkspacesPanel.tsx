// Owns the control-panel Workspaces list, search, rename form,
// delete confirmation, and cross-operation overflow focus restoration.
// Does not fetch or persist workspace layouts, own rail order, or own
// overflow measurement/placement — those live in
// workspace-overflow-menu-geometry.ts and WorkspaceRowOverflowMenu.tsx.
// Confirmation description ids use their own useId namespace, not a
// -confirm suffix on the row namespace. Split from the removed
// WorkspaceSwitcher in workspace-shell-controls.tsx.
// Mount refresh is one-shot via a ref: a new onRefresh identity does
// not refetch. A real remount still calls the latest callback with
// preserveError. Explicit Refresh/Reload always use the current
// onRefresh and do not forward the click event as refresh options.
// Owned-delete pending/generation/observed lifecycle lives in
// use-owned-workspace-deletes.ts. This file keeps confirm UI,
// retained summaries, and overflow focus restoration.
// Scalar edit/delete mirrors publish through useCommittedRef. Live
// retained summaries publish in a commit-phase insertion effect, never
// during render. Fallback uses a retained summary only when its id
// matches the current edit/confirm owner.

import {
  useEffect,
  useId,
  useInsertionEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";

import type { WorkspaceLayoutSummary } from "../api";
import type { WorkspaceDeleteRequest } from "../workspace-delete-request";
import { useCommittedRef } from "./use-committed-ref";
import { useOwnedWorkspaceDeletes } from "./use-owned-workspace-deletes";
import {
  isIgnorableEscape,
  WorkspaceRowOverflowMenu,
} from "./WorkspaceRowOverflowMenu";
import "./WorkspacesPanel.css";

export { workspaceOverflowTriggerLabel } from "./WorkspaceRowOverflowMenu";

export function formatWorkspaceShortId(workspaceId: string) {
  const normalized = workspaceId.trim();
  if (normalized.startsWith("workspace-") && normalized.length > "workspace-".length + 8) {
    return normalized.slice("workspace-".length, "workspace-".length + 8);
  }

  return normalized;
}

export function visibleWorkspaceSummaries(
  summaries: readonly WorkspaceLayoutSummary[],
  currentWorkspaceId: string,
): WorkspaceLayoutSummary[] {
  const byId = new Map(summaries.map((summary) => [summary.id, summary]));
  if (!byId.has(currentWorkspaceId)) {
    byId.set(currentWorkspaceId, {
      id: currentWorkspaceId,
      revision: 0,
      updatedAt: "",
      controlPanelSide: "left",
    });
  }

  return [...byId.values()].sort((left, right) => {
    if (left.id === right.id) return 0;
    if (left.id === currentWorkspaceId) return -1;
    if (right.id === currentWorkspaceId) return 1;
    return (left.label || left.id).localeCompare(right.label || right.id)
      || left.id.localeCompare(right.id);
  });
}

export function workspaceDisplayName(summary: Pick<WorkspaceLayoutSummary, "id" | "label">) {
  const label = summary.label?.trim() ?? "";
  return label || formatWorkspaceShortId(summary.id);
}

export function workspaceDescriptionDomId(instanceId: string, workspaceId: string) {
  const encoded = [...workspaceId]
    .map((char) => (/[A-Za-z0-9-]/.test(char) ? char : `_${char.codePointAt(0)!.toString(16)}_`))
    .join("");
  return `${instanceId}-${encoded}`;
}

function shouldRestoreLostFocus() {
  const active = document.activeElement;
  return !active || active === document.body || !document.contains(active);
}

export function workspacesRefreshBusy(
  isLoading: boolean,
  deletingWorkspaceIds: readonly string[],
) {
  return isLoading || deletingWorkspaceIds.length > 0;
}

export function publishLiveWorkspaceSummary(
  retainedRef: { current: WorkspaceLayoutSummary | null },
  live: WorkspaceLayoutSummary | null,
) {
  if (live) {
    retainedRef.current = live;
  }
}

export function matchingRetainedWorkspaceSummary(
  live: WorkspaceLayoutSummary | null,
  retained: WorkspaceLayoutSummary | null,
  ownerId: string | null,
  allowFallback: boolean,
): WorkspaceLayoutSummary | null {
  if (live) {
    return live;
  }
  if (allowFallback && ownerId && retained?.id === ownerId) {
    return retained;
  }
  return null;
}

export function WorkspacesPanelHeaderActions({
  isRefreshing = false,
  onOpenNewWorkspaceHere,
  onOpenNewWorkspaceWindow,
  onRefresh,
}: {
  isRefreshing?: boolean;
  onOpenNewWorkspaceHere: () => void;
  onOpenNewWorkspaceWindow: () => void;
  onRefresh: () => void;
}) {
  return (
    <>
      <button
        className="control-panel-header-action control-panel-header-new-session-button"
        type="button"
        onClick={onOpenNewWorkspaceHere}
        aria-label="New workspace here"
        title="New workspace here"
      >
        <span
          className="control-panel-header-action-icon control-panel-header-action-icon-new"
          aria-hidden="true"
        >
          <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
            <circle cx="8" cy="8" r="6.5" fill="none" stroke="currentColor" strokeWidth="1.3" />
            <path d="M8 5v6M5 8h6" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" />
          </svg>
        </span>
      </button>
      <button
        className="control-panel-header-action control-panel-header-open-button"
        type="button"
        onClick={onOpenNewWorkspaceWindow}
        aria-label="New window"
        title="New window"
      >
        New window
      </button>
      <button
        className="control-panel-header-action control-panel-header-open-button"
        type="button"
        onClick={() => {
          if (isRefreshing) {
            return;
          }
          onRefresh();
        }}
        onKeyDown={(event) => {
          if (!isRefreshing) {
            return;
          }
          if (event.key === " " || event.key === "Enter") {
            event.preventDefault();
          }
        }}
        aria-disabled={isRefreshing || undefined}
        aria-label="Refresh workspaces"
        title="Refresh"
      >
        Refresh
      </button>
    </>
  );
}

export function WorkspacesPanel({
  currentWorkspaceId,
  deletingWorkspaceIds,
  error,
  isLoading,
  summaries,
  onDeleteWorkspace,
  onRenameWorkspace,
  onRefresh,
  onOpenWorkspace,
}: {
  currentWorkspaceId: string;
  deletingWorkspaceIds: readonly string[];
  error: string | null;
  isLoading: boolean;
  summaries: readonly WorkspaceLayoutSummary[];
  onDeleteWorkspace: (workspaceId: string) => WorkspaceDeleteRequest;
  onRenameWorkspace: (workspaceId: string, label: string) => Promise<void>;
  onRefresh: (options?: { preserveError?: boolean }) => void;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const searchId = useId();
  const descriptionInstanceId = useId();
  const confirmDescriptionInstanceId = useId();
  const [searchQuery, setSearchQuery] = useState("");
  const [menuWorkspaceId, setMenuWorkspaceId] = useState<string | null>(null);
  const [confirmingDeleteId, setConfirmingDeleteId] = useState<string | null>(null);
  const [editingWorkspaceId, setEditingWorkspaceId] = useState<string | null>(null);
  const [labelDraft, setLabelDraft] = useState("");
  const [isSavingLabel, setIsSavingLabel] = useState(false);
  const [labelError, setLabelError] = useState<string | null>(null);
  const confirmDialogRef = useRef<HTMLDivElement | null>(null);
  const confirmCancelRef = useRef<HTMLButtonElement | null>(null);
  const labelInputRef = useRef<HTMLInputElement | null>(null);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const overflowButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  const restoreFocusIdRef = useRef<string | null>(null);
  const pendingRestoreFocusIdRef = useRef<string | null>(null);
  const pendingRestoreLabelFocusRef = useRef(false);
  const retainedEditingSummaryRef = useRef<WorkspaceLayoutSummary | null>(null);
  const retainedConfirmingSummaryRef = useRef<WorkspaceLayoutSummary | null>(null);
  const isSavingLabelRef = useCommittedRef(isSavingLabel);
  const editingWorkspaceIdRef = useCommittedRef(editingWorkspaceId);

  const visibleSummaries = useMemo(
    () => visibleWorkspaceSummaries(summaries, currentWorkspaceId),
    [currentWorkspaceId, summaries],
  );
  const deletingWorkspaceIdSet = useMemo(
    () => new Set(deletingWorkspaceIds),
    [deletingWorkspaceIds],
  );
  const {
    isOwnedDeletePending,
    confirmOwnedDelete,
  } = useOwnedWorkspaceDeletes({
    deletingWorkspaceIds,
    onDeleteWorkspace,
    onOwnedDeletesSettled: (workspaceIds) => {
      let restoreAfterOwnedSuccess = false;
      for (const workspaceId of workspaceIds) {
        const stillPresent = visibleSummaries.some((summary) => summary.id === workspaceId);
        if (stillPresent) {
          continue;
        }
        if (confirmingDeleteId === workspaceId) {
          setConfirmingDeleteId(null);
          restoreAfterOwnedSuccess = true;
        }
      }
      if (restoreAfterOwnedSuccess && shouldRestoreLostFocus()) {
        focusSurvivingOverflow();
      }
    },
  });
  const normalizedQuery = searchQuery.trim().toLowerCase();
  const filteredSummaries = useMemo(() => {
    if (!normalizedQuery) {
      return visibleSummaries;
    }

    return visibleSummaries.filter((summary) => {
      const label = summary.label?.toLowerCase() ?? "";
      return label.includes(normalizedQuery) || summary.id.toLowerCase().includes(normalizedQuery);
    });
  }, [normalizedQuery, visibleSummaries]);
  const liveEditingSummary = visibleSummaries.find((summary) => summary.id === editingWorkspaceId) ?? null;
  const liveConfirmingSummary = visibleSummaries.find((summary) => summary.id === confirmingDeleteId) ?? null;
  useInsertionEffect(() => {
    publishLiveWorkspaceSummary(retainedEditingSummaryRef, liveEditingSummary);
    publishLiveWorkspaceSummary(retainedConfirmingSummaryRef, liveConfirmingSummary);
  }, [liveConfirmingSummary, liveEditingSummary]);
  const editingSummary = matchingRetainedWorkspaceSummary(
    liveEditingSummary,
    retainedEditingSummaryRef.current,
    editingWorkspaceId,
    Boolean(editingWorkspaceId && (isSavingLabel || labelError)),
  );
  const confirmingSummary = matchingRetainedWorkspaceSummary(
    liveConfirmingSummary,
    retainedConfirmingSummaryRef.current,
    confirmingDeleteId,
    Boolean(confirmingDeleteId && deletingWorkspaceIdSet.has(confirmingDeleteId)),
  );
  const confirmDescriptionId = confirmingSummary
    ? workspaceDescriptionDomId(confirmDescriptionInstanceId, confirmingSummary.id)
    : null;
  const didMountRefreshRef = useRef(false);
  const overflowLayoutSignature = [
    filteredSummaries.map((summary) => summary.id).join("\0"),
    editingWorkspaceId ?? "",
    confirmingDeleteId ?? "",
    error ?? "",
  ].join("|");

  useEffect(() => {
    if (didMountRefreshRef.current) {
      return;
    }
    didMountRefreshRef.current = true;
    onRefresh({ preserveError: true });
  }, [onRefresh]);

  useEffect(() => {
    if (!editingWorkspaceId) {
      return;
    }
    labelInputRef.current?.focus();
  }, [editingWorkspaceId]);

  useEffect(() => {
    if (!confirmingDeleteId) {
      return;
    }
    confirmCancelRef.current?.focus();
  }, [confirmingDeleteId]);

  useEffect(() => {
    if (!confirmingDeleteId) {
      return;
    }
    if (
      !isOwnedDeletePending(confirmingDeleteId)
      && !deletingWorkspaceIdSet.has(confirmingDeleteId)
    ) {
      return;
    }
    if (shouldRestoreLostFocus()) {
      confirmDialogRef.current?.focus();
    }
  }, [confirmingDeleteId, deletingWorkspaceIdSet, isOwnedDeletePending]);

  useEffect(() => {
    const restoreId = pendingRestoreFocusIdRef.current;
    if (!restoreId || isSavingLabel) {
      return;
    }
    pendingRestoreFocusIdRef.current = null;
    if (!shouldRestoreLostFocus()) {
      return;
    }
    const target = overflowButtonRefs.current.get(restoreId);
    if (target && !target.disabled) {
      target.focus();
      return;
    }
    focusSurvivingOverflow();
  }, [confirmingDeleteId, editingWorkspaceId, isSavingLabel, visibleSummaries]);

  useEffect(() => {
    if (isSavingLabel || !editingWorkspaceId || !pendingRestoreLabelFocusRef.current) {
      return;
    }
    const input = labelInputRef.current;
    if (!input || input.disabled) {
      return;
    }
    const active = document.activeElement;
    const form = input.form;
    const movedAway = Boolean(
      active
      && active !== document.body
      && document.contains(active)
      && active !== input
      && !(form && form.contains(active)),
    );
    pendingRestoreLabelFocusRef.current = false;
    if (movedAway) {
      return;
    }
    input.focus();
  }, [editingWorkspaceId, isSavingLabel, labelError]);

  useEffect(() => {
    if (!menuWorkspaceId) {
      return;
    }
    if (visibleSummaries.some((summary) => summary.id === menuWorkspaceId)) {
      return;
    }
    setMenuWorkspaceId(null);
  }, [menuWorkspaceId, visibleSummaries]);

  useEffect(() => {
    if (!editingWorkspaceId) {
      return;
    }
    if (visibleSummaries.some((summary) => summary.id === editingWorkspaceId)) {
      return;
    }
    if (isSavingLabel || labelError) {
      return;
    }
    setEditingWorkspaceId(null);
    setLabelDraft("");
    retainedEditingSummaryRef.current = null;
  }, [editingWorkspaceId, isSavingLabel, labelError, visibleSummaries]);

  useEffect(() => {
    if (!confirmingDeleteId) {
      return;
    }
    if (visibleSummaries.some((summary) => summary.id === confirmingDeleteId)) {
      return;
    }
    if (
      isOwnedDeletePending(confirmingDeleteId)
      || deletingWorkspaceIdSet.has(confirmingDeleteId)
    ) {
      return;
    }
    setConfirmingDeleteId(null);
    retainedConfirmingSummaryRef.current = null;
  }, [confirmingDeleteId, deletingWorkspaceIdSet, isOwnedDeletePending, visibleSummaries]);

  function focusOverflow(workspaceId: string | null) {
    if (!workspaceId) {
      return;
    }
    overflowButtonRefs.current.get(workspaceId)?.focus();
  }

  function focusSurvivingOverflow() {
    const nextOverflow = visibleSummaries.find((summary) => {
      const button = overflowButtonRefs.current.get(summary.id);
      return Boolean(button && !button.disabled);
    });
    if (nextOverflow) {
      focusOverflow(nextOverflow.id);
      return;
    }
    searchInputRef.current?.focus();
  }

  function cancelRename(workspaceId: string) {
    pendingRestoreFocusIdRef.current = workspaceId;
    setEditingWorkspaceId(null);
    setLabelError(null);
    retainedEditingSummaryRef.current = null;
    pendingRestoreLabelFocusRef.current = false;
  }

  function cancelDeleteConfirmation(workspaceId: string) {
    if (isOwnedDeletePending(workspaceId) || deletingWorkspaceIdSet.has(workspaceId)) {
      return;
    }
    pendingRestoreFocusIdRef.current = workspaceId;
    setConfirmingDeleteId((current) => (current === workspaceId ? null : current));
  }

  function handlePanelKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
    if (isIgnorableEscape(event)) {
      return;
    }
    if (menuWorkspaceId) {
      event.preventDefault();
      const restoreId = restoreFocusIdRef.current ?? menuWorkspaceId;
      setMenuWorkspaceId(null);
      focusOverflow(restoreId);
      return;
    }
    if (confirmingSummary) {
      if (
        isOwnedDeletePending(confirmingSummary.id)
        || deletingWorkspaceIdSet.has(confirmingSummary.id)
      ) {
        return;
      }
      event.preventDefault();
      cancelDeleteConfirmation(confirmingSummary.id);
      return;
    }
    if (editingSummary && !isSavingLabelRef.current) {
      event.preventDefault();
      cancelRename(editingSummary.id);
    }
  }

  async function saveLabel() {
    if (!editingWorkspaceId || isSavingLabel) {
      return;
    }
    const savedId = editingWorkspaceId;
    const savedLabel = labelDraft.trim();
    setIsSavingLabel(true);
    setLabelError(null);
    try {
      await onRenameWorkspace(savedId, savedLabel);
      if (editingWorkspaceIdRef.current !== savedId) {
        return;
      }
      pendingRestoreFocusIdRef.current = savedId;
      setEditingWorkspaceId(null);
    } catch (saveError) {
      if (editingWorkspaceIdRef.current !== savedId) {
        return;
      }
      pendingRestoreLabelFocusRef.current = true;
      setLabelError(saveError instanceof Error ? saveError.message : String(saveError));
    } finally {
      setIsSavingLabel(false);
    }
  }

  function startRename(summary: WorkspaceLayoutSummary) {
    if (isSavingLabelRef.current) {
      return;
    }
    restoreFocusIdRef.current = summary.id;
    if (pendingRestoreFocusIdRef.current !== summary.id) {
      pendingRestoreFocusIdRef.current = null;
    }
    setMenuWorkspaceId(null);
    setConfirmingDeleteId(null);
    setEditingWorkspaceId(summary.id);
    setLabelDraft(summary.label ?? "");
    setLabelError(null);
  }

  function requestDelete(workspaceId: string) {
    restoreFocusIdRef.current = workspaceId;
    setMenuWorkspaceId(null);
    setConfirmingDeleteId(workspaceId);
  }

  return (
    <section
      className="workspaces-panel"
      role="region"
      aria-label="Workspaces"
      onKeyDown={handlePanelKeyDown}
    >
      <div className="workspaces-panel-toolbar">
        <label className="visually-hidden" htmlFor={searchId}>
          Search workspaces
        </label>
        <input
          ref={searchInputRef}
          id={searchId}
          className="themed-input workspaces-panel-search"
          type="search"
          value={searchQuery}
          placeholder="Search by name or id"
          spellCheck={false}
          onChange={(event) => setSearchQuery(event.currentTarget.value)}
        />
      </div>

      {editingSummary ? (
        <form
          className="workspace-label-form"
          aria-label={`Label workspace ${editingSummary.id}`}
          onSubmit={(event) => {
            event.preventDefault();
            void saveLabel();
          }}
        >
          <label>
            Workspace label
            <input
              ref={labelInputRef}
              key={editingSummary.id}
              autoFocus
              maxLength={80}
              value={labelDraft}
              disabled={isSavingLabel && editingWorkspaceId === editingSummary.id}
              placeholder="For example: Backend, Reviews, Planning"
              onChange={(event) => setLabelDraft(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (isIgnorableEscape(event) || isSavingLabelRef.current) {
                  return;
                }
                event.preventDefault();
                event.stopPropagation();
                cancelRename(editingSummary.id);
              }}
            />
          </label>
          <div className="workspaces-panel-form-actions">
            <button className="ghost-button" type="submit" disabled={isSavingLabel}>
              {isSavingLabel ? "Saving…" : "Save label"}
            </button>
            <button
              className="ghost-button"
              type="button"
              disabled={isSavingLabel}
              onClick={() => {
                cancelRename(editingSummary.id);
              }}
            >
              Cancel
            </button>
          </div>
          <p className="workspaces-panel-status">Leave empty to remove the label.</p>
          {labelError ? <p className="workspaces-panel-error" role="alert">{labelError}</p> : null}
        </form>
      ) : null}

      {confirmingSummary && confirmDescriptionId ? (
        <div
          ref={confirmDialogRef}
          className="workspaces-panel-confirm"
          role="group"
          tabIndex={-1}
          aria-label={`Delete workspace ${confirmingSummary.id}`}
          aria-describedby={confirmDescriptionId}
        >
          <p id={confirmDescriptionId}>
            Delete {workspaceDisplayName(confirmingSummary)}? This removes the saved layout.
          </p>
          <div className="workspaces-panel-form-actions">
            <button
              className="ghost-button workspaces-panel-confirm-delete"
              type="button"
              aria-disabled={deletingWorkspaceIdSet.has(confirmingSummary.id)}
              onClick={() => {
                confirmOwnedDelete(confirmingSummary.id);
              }}
            >
              {deletingWorkspaceIdSet.has(confirmingSummary.id) ? "Deleting" : "Confirm delete"}
            </button>
            <button
              ref={confirmCancelRef}
              className="ghost-button"
              type="button"
              aria-disabled={deletingWorkspaceIdSet.has(confirmingSummary.id)}
              onClick={() => {
                cancelDeleteConfirmation(confirmingSummary.id);
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : null}

      <div className="workspaces-panel-list" role="list">
        {filteredSummaries.map((summary) => {
          const isCurrent = summary.id === currentWorkspaceId;
          const isDeleting = deletingWorkspaceIdSet.has(summary.id);
          const isPersisted = summary.revision > 0;
          const title = workspaceDisplayName(summary);
          const menuOpen = menuWorkspaceId === summary.id;
          return (
            <div
              key={summary.id}
              className="workspaces-panel-item"
              role="listitem"
            >
              <button
                className={`workspaces-panel-switch ${isCurrent ? "selected" : ""}`}
                type="button"
                title={summary.id}
                aria-label={isCurrent ? `${title} (current)` : title}
                aria-current={isCurrent ? "true" : undefined}
                aria-describedby={workspaceDescriptionDomId(descriptionInstanceId, summary.id)}
                onClick={() => onOpenWorkspace(summary.id)}
              >
                <span className="workspaces-panel-item-copy">
                  <span className="workspaces-panel-item-title-row">
                    <span className="workspaces-panel-item-title">{title}</span>
                    {isCurrent ? (
                      <span className="workspaces-panel-item-status">Current</span>
                    ) : null}
                  </span>
                  {summary.updatedAt ? (
                    <span className="workspaces-panel-item-meta">{summary.updatedAt}</span>
                  ) : null}
                </span>
              </button>
              <span id={workspaceDescriptionDomId(descriptionInstanceId, summary.id)} className="visually-hidden">
                {summary.id}
              </span>
              {isPersisted || !isCurrent ? (
                <WorkspaceRowOverflowMenu
                  workspaceId={summary.id}
                  displayName={title}
                  descriptionId={workspaceDescriptionDomId(descriptionInstanceId, summary.id)}
                  isCurrent={isCurrent}
                  isPersisted={isPersisted}
                  isDeleting={isDeleting}
                  isSavingLabel={isSavingLabel}
                  open={menuOpen}
                  layoutSignature={overflowLayoutSignature}
                  onToggle={() => {
                    restoreFocusIdRef.current = summary.id;
                    setMenuWorkspaceId((current) => (current === summary.id ? null : summary.id));
                  }}
                  onClose={() => {
                    setMenuWorkspaceId((current) => (current === summary.id ? null : current));
                  }}
                  onRename={() => startRename(summary)}
                  onRequestDelete={() => requestDelete(summary.id)}
                  onTriggerRef={(node) => {
                    if (node) {
                      overflowButtonRefs.current.set(summary.id, node);
                    } else {
                      overflowButtonRefs.current.delete(summary.id);
                    }
                  }}
                  onEscapeToTrigger={() => {
                    setMenuWorkspaceId(null);
                    focusOverflow(summary.id);
                  }}
                />
              ) : null}
            </div>
          );
        })}
      </div>

      {normalizedQuery && filteredSummaries.length === 0 ? (
        <p className="workspaces-panel-status">No workspaces match that search.</p>
      ) : null}
      <p
        className={`workspaces-panel-status workspaces-panel-live-status${
          isLoading || deletingWorkspaceIds.length > 0 ? "" : " visually-hidden"
        }`}
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        {isLoading
          ? "Loading saved workspaces…"
          : deletingWorkspaceIds.length > 0
            ? "Deleting"
            : ""}
      </p>
      {error ? (
        <div className="workspaces-panel-error">
          <p role="alert">{error}</p>
          <button
            className="ghost-button workspaces-panel-retry"
            type="button"
            onClick={() => onRefresh()}
          >
            Reload list
          </button>
        </div>
      ) : null}
    </section>
  );
}
