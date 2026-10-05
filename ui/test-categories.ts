/*
The UI test categories: one manifest that vite.config.ts and the category
guard (src/test-categories.test.ts) both read.

Owns: which category each UI test file belongs to, the resource tags of the
heavy files, and the four Vitest projects built from them (unit, component,
heavy, app, run in that order, one file at a time), with the test
environment of each: node for unit, jsdom for the others.
  - app: renders the whole App (explicit list).
  - heavy: a component file with a test at or above 2 s in measured runs,
    with real-time sensitivity, or that exercises the virtualizer and
    measurement lifecycle (explicit list, with tags).
  - unit: a *.test.ts file that needs no DOM (explicit list), run without one.
  - component: every other *.test.tsx, plus the *.test.ts files listed as
    needing the DOM.
Every *.test.ts is in exactly one of the unit, DOM and app lists, so a new one
is placed on purpose; the guard fails until it is.
Does not own: running a category or accounting a run per project (the
launcher's `category` mode and scripts/test-categories-plan.mjs, which read
CATEGORY_PROJECTS and projectSelects from here), choosing what runs in a
review round, or the per-test duration report (scripts/test-durations.mjs).
New file; the two projects it replaces were inline in vite.config.ts.
*/

export type TestCategory = "unit" | "component" | "heavy" | "app";

// cpu-heavy: a test at or above the 2 s budget in measured runs.
// wall-clock: waits in real time (waitFor or a real timer without fake timers).
export type ResourceTag = "cpu-heavy" | "wall-clock";

export type HeavyTestFile = {
  readonly file: string;
  readonly tags: readonly ResourceTag[];
  readonly reason: string;
};

export const APP_TEST_FILES: readonly string[] = [
  "src/App.control-panel-dnd.test.tsx",
  "src/App.control-panel.board.test.tsx",
  "src/App.control-panel.openers.test.tsx",
  "src/App.control-panel.scoping.test.tsx",
  "src/App.control-panel.test.tsx",
  "src/App.control-panel.work.test.tsx",
  "src/App.diff-preview.test.tsx",
  "src/App.legacy-body-loss-parity.test.tsx",
  "src/App.live-state.delta-watchdog.test.tsx",
  "src/App.live-state.deltas.test.tsx",
  "src/App.live-state.hydration-recovery.test.tsx",
  "src/App.live-state.hydration.test.tsx",
  "src/App.live-state.reconnect.test.tsx",
  "src/App.live-state.restart-roundtrip.test.tsx",
  "src/App.live-state.sparse-tail.test.tsx",
  "src/App.live-state.visibility.test.tsx",
  "src/App.live-state.watchdog.test.tsx",
  "src/App.orchestrators.test.tsx",
  "src/App.preferences.test.tsx",
  "src/App.scroll-behavior.test.tsx",
  "src/App.send-scroll.test.tsx",
  "src/App.session-lifecycle.test.tsx",
  "src/App.shared-live-events.test.tsx",
  "src/App.smoke.test.tsx",
  "src/App.transcript-loss-authority.test.tsx",
  "src/App.workspace-files-changed.test.tsx",
  "src/App.workspace-layout.test.tsx",
  "src/SessionPaneView.retry-display.test.tsx",
  "src/app-test-harness.test.tsx",
  "src/backend-connection.test.tsx",
];

export const HEAVY_TEST_FILES: readonly HeavyTestFile[] = [
  {
    file: "src/panels/VirtualizedConversationMessageList.test.tsx",
    tags: ["cpu-heavy", "wall-clock"],
    reason: "virtualizer; tests at or above 2 s; waitFor in real time",
  },
  {
    file: "src/panels/WorkPanel.test.tsx",
    tags: ["cpu-heavy", "wall-clock"],
    reason: "tests at or above 2 s; waitFor without fake timers",
  },
  {
    file: "src/panels/WorkTree.test.tsx",
    tags: ["cpu-heavy"],
    reason: "4,000-row fixture at about 2 s",
  },
  {
    file: "src/panels/AgentSessionPanel.virtualization.test.tsx",
    tags: ["wall-clock"],
    reason: "virtualizer lifecycle; waitFor in real time",
  },
  {
    file: "src/panels/AgentSessionPanel.virtualization-scroll.test.tsx",
    tags: ["wall-clock"],
    reason: "virtualizer lifecycle; waitFor in real time",
  },
  {
    file: "src/SessionPaneView.delegation-composer.test.tsx",
    tags: ["wall-clock"],
    reason: "scheduler-sensitive SessionPaneView lifecycle; waitFor without fake timers",
  },
  {
    file: "src/SessionPaneView.scroll-idle-measurement.test.tsx",
    tags: [],
    reason: "measurement lifecycle",
  },
  {
    file: "src/SessionPaneView.scroll-native-authority.test.tsx",
    tags: [],
    reason: "measurement lifecycle",
  },
];

// Files that need no DOM. The unit project runs them in a node environment,
// so a file whose run reaches a DOM global node lacks, such as document or
// window, itself or through a module it imports, fails; such a file belongs
// in the list below. Browser storage is not caught that way, since
// test-setup supplies in-memory storage in every project, and neither is code
// behind a typeof check. The guard's static check is a quicker backstop that
// reads only each test file's own text for its listed patterns.
export const UNIT_TEST_FILES: readonly string[] = [
  "src/SessionPaneView.active-tab.test.ts",
  "src/SessionPaneView.content-transition.test.ts",
  "src/SessionPaneView.delegation-waits.test.ts",
  "src/SessionPaneView.messages.test.ts",
  "src/SessionPaneView.scroll-key.test.ts",
  "src/action-state-adoption.test.ts",
  "src/api-request.body.test.ts",
  "src/api-request.test.ts",
  "src/api-terminal.test.ts",
  "src/api.test.ts",
  "src/app-live-state-activity.test.ts",
  "src/app-live-state-delegation-waits.test.ts",
  "src/app-live-state-delta-events.test.ts",
  "src/app-live-state-model-confirmations.test.ts",
  "src/app-live-state-reconnect-state.test.ts",
  "src/app-live-state-resync-options.test.ts",
  "src/app-live-state-transport.test.ts",
  "src/app-live-state-workspace-reconciliation.test.ts",
  "src/app-session-draft-attachments.test.ts",
  "src/app-session-draft-sync.test.ts",
  "src/app-session-model-requests.test.ts",
  "src/app-session-settings-optimism.test.ts",
  "src/app-session-settings-payload.test.ts",
  "src/bounded-content-equality.test.ts",
  "src/connection-retry.test.ts",
  "src/control-surface-state.test.ts",
  "src/conversation-marker-colors.test.ts",
  "src/conversation-marker-requests.test.ts",
  "src/conversation-marker-response-match.test.ts",
  "src/conversation-marker-session-mutations.test.ts",
  "src/conversation-marker-state-equality.test.ts",
  "src/delegation-error-packets.test.ts",
  "src/delegation-fan-in.test.ts",
  "src/delegation-result-prompt.test.ts",
  "src/diff-preview.test.ts",
  "src/error-messages.test.ts",
  "src/highlight.test.ts",
  "src/long-peer-message.test.ts",
  "src/mailbox-presentation.test.ts",
  "src/markdown-bare-file-autolinks.test.ts",
  "src/markdown-links.test.ts",
  "src/markdown-streaming-split.test.ts",
  "src/optimistic-pending-prompt.test.ts",
  "src/pane-scroll-position-migration.test.ts",
  "src/pane-tab-status-tooltip.test.ts",
  "src/panels/AgentSessionPanel.waiting-indicator.test.ts",
  "src/panels/OrchestratorTemplatesPanel.geometry.test.ts",
  "src/panels/agent-session-panel-helpers.test.ts",
  "src/panels/git-status-tree.test.ts",
  "src/panels/queue-pause-cause.test.ts",
  "src/panels/session-agent-command-submission.test.ts",
  "src/panels/session-slash-palette.test.ts",
  "src/panels/session-tab-status-tooltip.test.ts",
  "src/panels/use-response-board-tabs.test.ts",
  "src/panels/virtualized-conversation-measurement.test.ts",
  "src/panels/work-labels.test.ts",
  "src/panels/work-sort.test.ts",
  "src/panels/work-tree.test.ts",
  "src/path-display.test.ts",
  "src/remotes.test.ts",
  "src/response-board-navigation.test.ts",
  "src/response-board.test.ts",
  "src/session-drag.test.ts",
  "src/session-find.test.ts",
  "src/session-hydration-adoption.test.ts",
  "src/session-list-filter.test.ts",
  "src/session-model-options.test.ts",
  "src/session-model-utils.test.ts",
  "src/session-publication-boundary.test.ts",
  "src/session-store.test.ts",
  "src/state-revision.test.ts",
  "src/test-categories.test.ts",
  "src/test-runs-api.test.ts",
  "src/test-runs.test.ts",
  "src/transcript-body-placement.test.ts",
  "src/transcript-body-sequence.test.ts",
  "src/transcript-generated-model.test.ts",
  "src/transcript-repair-authority.test.ts",
  "src/wait-delta-watermark.test.ts",
  "src/workspace-file-events.test.ts",
  "src/workspace-mailbox.test.ts",
  "src/workspace-pane-routing.test.ts",
  "src/workspace-response-board.test.ts",
  "src/workspace-test-runs.test.ts",
  "src/workspace-work.test.ts",
];

// *.test.ts files that need the DOM; they run with the component files.
export const DOM_TS_TEST_FILES: readonly string[] = [
  "src/MonacoCodeEditor.test.ts",
  "src/SessionPaneView.render-callbacks.test.ts",
  "src/SessionPaneView.resize-measurement.test.ts",
  "src/SessionPaneView.scroll-boundaries.test.ts",
  "src/SessionPaneView.scroll-observers.test.ts",
  "src/SessionPaneView.scroll.keyboard.test.ts",
  "src/SessionPaneView.scroll.test.ts",
  "src/active-prompt-poll.test.ts",
  "src/app-live-state-wait-repair.test.ts",
  "src/app-live-state-workspace-events.test.ts",
  "src/app-live-state.test.ts",
  "src/app-session-actions.test.ts",
  "src/app-utils.test.ts",
  "src/browser-platform.test.ts",
  "src/clipboard.test.ts",
  "src/delegation-commands.test.ts",
  "src/dialog-backdrop-dismiss.test.ts",
  "src/live-updates.test.ts",
  "src/mermaid-render.test.ts",
  "src/mermaid-theme-override.test.ts",
  "src/message-stack-scroll-sync.test.ts",
  "src/message-stack-viewport-clamp.test.ts",
  "src/monaco-cancellation-filter.test.ts",
  "src/monaco-theme.test.ts",
  "src/pane-keyboard.test.ts",
  "src/panels/conversation-markers.test.ts",
  "src/panels/conversation-message-reveal.test.ts",
  "src/panels/conversation-overview-map.test.ts",
  "src/panels/conversation-virtualization.test.ts",
  "src/panels/markdown-commit-ranges.test.ts",
  "src/panels/markdown-diff-change-index.test.ts",
  "src/panels/markdown-diff-clipboard-pointer.test.ts",
  "src/panels/markdown-diff-edit-pipeline.test.ts",
  "src/panels/markdown-diff-segments.test.ts",
  "src/panels/response-board-source-navigation.test.ts",
  "src/panels/use-owned-workspace-deletes.test.ts",
  "src/panels/use-workspace-rename-editor.test.ts",
  "src/panels/useInitialActiveTranscriptMessages.test.ts",
  "src/panels/virtualized-conversation-scroll-generation.test.ts",
  "src/session-history-demand.test.ts",
  "src/session-history-loading.test.ts",
  "src/session-history.test.ts",
  "src/session-hydration-performance.test.ts",
  "src/session-pane-detached-restore.test.ts",
  "src/session-reconcile.test.ts",
  "src/shared-live-events.test.ts",
  "src/source-renderers.test.ts",
  "src/tab-drag.test.ts",
  "src/test-setup.test.ts",
  "src/themes.test.ts",
  // Reads production source and names `window.setInterval` in its AST
  // checks, which the guard's text scan for unit files rejects.
  "src/transcript-proof-boundary.test.ts",
  "src/use-stable-map-by-signature.test.ts",
  "src/use-theme-preferences.test.ts",
  "src/workspace-storage.test.ts",
  "src/workspace-viewer-layout.test.ts",
  "src/workspace.test.ts",
];

// The only glob in the manifest; every other selection is an exact path.
export const COMPONENT_TSX_GLOB = "src/**/*.test.tsx";

export type CategoryProject = {
  readonly name: TestCategory;
  readonly include: readonly string[];
  readonly exclude: readonly string[];
  // Distinct per project: Vitest runs projects that share a groupOrder at
  // the same time, so distinct values keep one file at a time for the stage.
  // Never 0: Vitest 4 moves a project with groupOrder 0, one worker and
  // isolation into a group of its own that runs after every ordered group,
  // so a 0 here would run that project last.
  readonly groupOrder: number;
  // node has no DOM, which is the unit contract; jsdom supplies one.
  readonly environment: "node" | "jsdom";
};

const heavyFiles = HEAVY_TEST_FILES.map(({ file }) => file);

export const CATEGORY_PROJECTS: readonly CategoryProject[] = [
  {
    name: "unit",
    include: UNIT_TEST_FILES,
    exclude: [],
    groupOrder: 1,
    environment: "node",
  },
  {
    name: "component",
    include: [COMPONENT_TSX_GLOB, ...DOM_TS_TEST_FILES],
    exclude: [...APP_TEST_FILES, ...heavyFiles],
    groupOrder: 2,
    environment: "jsdom",
  },
  { name: "heavy", include: heavyFiles, exclude: [], groupOrder: 3, environment: "jsdom" },
  { name: "app", include: APP_TEST_FILES, exclude: [], groupOrder: 4, environment: "jsdom" },
];

function matchesSelection(pattern: string, file: string): boolean {
  if (pattern === COMPONENT_TSX_GLOB) {
    return /^src\/.+\.test\.tsx$/.test(file);
  }
  return pattern === file;
}

// Whether a project selects a test file (a path relative to ui/, with forward
// slashes), using the same include and exclude lists the config hands Vitest.
export function projectSelects(project: CategoryProject, file: string): boolean {
  return project.include.some((pattern) => matchesSelection(pattern, file)) &&
    !project.exclude.some((pattern) => matchesSelection(pattern, file));
}
