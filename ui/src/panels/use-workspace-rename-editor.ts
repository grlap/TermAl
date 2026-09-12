// Owns pending/editor state for workspace rename PATCH
// operations, including the summary captured once at
// startRename. That snapshot must survive Workspaces unmount,
// dock-side moves, and row removal; later live metadata is
// not copied into it. Evolved from WorkspacesPanel.tsx
// rename lifetime, not a pure split. Lives on App above the
// movable AppControlSurface. Does not own confirm UI,
// overflow focus application, list refresh, or the rename
// HTTP adapter, and does not survive App unmount or browser
// reload. Restore intent refs stay private; take/control
// callbacks are stable. The returned object and saveLabel
// are not. Delete-confirm cancellation lives in the panel.
// Only one WorkspacesPanel can mount (no standalone
// Workspaces launcher), so a single mounted-view flag is
// enough.

import { useCallback, useRef, useState } from "react";

import type { WorkspaceLayoutSummary } from "../api";
import { useCommittedRef } from "./use-committed-ref";

export type WorkspaceRenameEditor = {
  editingWorkspaceId: string | null;
  editingSummarySnapshot: WorkspaceLayoutSummary | null;
  labelDraft: string;
  isSavingLabel: boolean;
  labelError: string | null;
  isSaveLocked: () => boolean;
  setLabelDraft: (value: string) => void;
  setEditorViewMounted: (mounted: boolean) => void;
  startRename: (summary: WorkspaceLayoutSummary) => boolean;
  cancelRename: (workspaceId: string) => void;
  clearUnlockedRename: () => void;
  takePendingRestoreFocusId: () => string | null;
  takePendingRestoreLabelFocus: () => boolean;
  saveLabel: () => Promise<void>;
};

export function useWorkspaceRenameEditor({
  onRenameWorkspace,
}: {
  onRenameWorkspace: (workspaceId: string, label: string) => Promise<void>;
}): WorkspaceRenameEditor {
  const [editingWorkspaceId, setEditingWorkspaceId] = useState<string | null>(null);
  const [editingSummarySnapshot, setEditingSummarySnapshot] = useState<
    WorkspaceLayoutSummary | null
  >(null);
  const [labelDraft, setLabelDraft] = useState("");
  const [isSavingLabel, setIsSavingLabel] = useState(false);
  const [labelError, setLabelError] = useState<string | null>(null);
  const pendingRestoreFocusIdRef = useRef<string | null>(null);
  const pendingRestoreLabelFocusRef = useRef(false);
  const saveLockRef = useRef(false);
  const editorViewMountedRef = useRef(false);
  const editingWorkspaceIdRef = useCommittedRef(editingWorkspaceId);
  const labelDraftRef = useCommittedRef(labelDraft);

  const isSaveLocked = useCallback(() => saveLockRef.current, []);

  const takePendingRestoreFocusId = useCallback(() => {
    if (saveLockRef.current) {
      return null;
    }
    const restoreId = pendingRestoreFocusIdRef.current;
    pendingRestoreFocusIdRef.current = null;
    return restoreId;
  }, []);

  const takePendingRestoreLabelFocus = useCallback(() => {
    if (saveLockRef.current) {
      return false;
    }
    const pending = pendingRestoreLabelFocusRef.current;
    pendingRestoreLabelFocusRef.current = false;
    return pending;
  }, []);

  const setEditorViewMounted = useCallback((mounted: boolean) => {
    editorViewMountedRef.current = mounted;
    if (mounted) {
      return;
    }
    pendingRestoreFocusIdRef.current = null;
    pendingRestoreLabelFocusRef.current = false;
  }, []);

  const startRename = useCallback((summary: WorkspaceLayoutSummary) => {
    if (saveLockRef.current) {
      return false;
    }
    if (pendingRestoreFocusIdRef.current !== summary.id) {
      pendingRestoreFocusIdRef.current = null;
    }
    pendingRestoreLabelFocusRef.current = false;
    setEditingWorkspaceId(summary.id);
    setEditingSummarySnapshot(summary);
    setLabelDraft(summary.label ?? "");
    setLabelError(null);
    return true;
  }, []);

  const cancelRename = useCallback((workspaceId: string) => {
    if (saveLockRef.current) {
      return;
    }
    pendingRestoreFocusIdRef.current = workspaceId;
    pendingRestoreLabelFocusRef.current = false;
    setEditingWorkspaceId(null);
    setEditingSummarySnapshot(null);
    setLabelError(null);
  }, []);

  const clearUnlockedRename = useCallback(() => {
    if (saveLockRef.current) {
      return;
    }
    pendingRestoreLabelFocusRef.current = false;
    setEditingWorkspaceId(null);
    setEditingSummarySnapshot(null);
    setLabelDraft("");
    setLabelError(null);
  }, []);

  async function saveLabel() {
    const savedId = editingWorkspaceIdRef.current;
    if (!savedId || saveLockRef.current) {
      return;
    }
    const savedLabel = labelDraftRef.current.trim();
    saveLockRef.current = true;
    setIsSavingLabel(true);
    setLabelError(null);
    try {
      await onRenameWorkspace(savedId, savedLabel);
      if (editorViewMountedRef.current) {
        pendingRestoreFocusIdRef.current = savedId;
      }
      setEditingWorkspaceId(null);
      setEditingSummarySnapshot(null);
    } catch (saveError) {
      if (editorViewMountedRef.current) {
        pendingRestoreLabelFocusRef.current = true;
      }
      setLabelError(saveError instanceof Error ? saveError.message : String(saveError));
    } finally {
      saveLockRef.current = false;
      setIsSavingLabel(false);
    }
  }

  return {
    editingWorkspaceId,
    editingSummarySnapshot,
    labelDraft,
    isSavingLabel,
    labelError,
    isSaveLocked,
    setLabelDraft,
    setEditorViewMounted,
    startRename,
    cancelRename,
    clearUnlockedRename,
    takePendingRestoreFocusId,
    takePendingRestoreLabelFocus,
    saveLabel,
  };
}
