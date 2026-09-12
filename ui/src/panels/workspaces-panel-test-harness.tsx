// Owns a test-only wrapper that supplies the required rename
// editor hook. Does not own panel UI, App dock wiring, or
// production rename lifetime. Used by WorkspacesPanel,
// workspace-labels, and workspace-links tests.

import { useWorkspaceRenameEditor } from "./use-workspace-rename-editor";
import { WorkspacesPanel } from "./WorkspacesPanel";

export type WorkspacesPanelHarnessProps = Omit<
  Parameters<typeof WorkspacesPanel>[0],
  "renameEditor"
> & {
  onRenameWorkspace: (workspaceId: string, label: string) => Promise<void>;
};

export function WorkspacesPanelHarness({
  onRenameWorkspace,
  ...props
}: WorkspacesPanelHarnessProps) {
  const renameEditor = useWorkspaceRenameEditor({ onRenameWorkspace });
  return <WorkspacesPanel {...props} renameEditor={renameEditor} />;
}

export function PersistentWorkspacesPanelHarness({
  showPanel,
  onRenameWorkspace,
  ...props
}: WorkspacesPanelHarnessProps & { showPanel: boolean }) {
  const renameEditor = useWorkspaceRenameEditor({ onRenameWorkspace });
  return (
    <>
      <button type="button">Unrelated control</button>
      {showPanel ? <WorkspacesPanel {...props} renameEditor={renameEditor} /> : null}
    </>
  );
}
