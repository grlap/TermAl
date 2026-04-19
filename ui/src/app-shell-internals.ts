// Constants and types that live at App.tsx's module scope.
//
// What this file owns:
//   - Timing constants (`TAB_DRAG_STALE_TIMEOUT_MS`,
//     `RECONNECT_STATE_RESYNC_DELAY_MS`,
//     `RECONNECT_STATE_RESYNC_MAX_DELAY_MS`,
//     `LIVE_SESSION_RESUME_WATCHDOG_INTERVAL_MS`,
//     `WORKSPACE_LAYOUT_PERSIST_DELAY_MS`,
//     `PENDING_KILL_CLOSE_DELAY_MS`,
//     `PENDING_SESSION_RENAME_CLOSE_DELAY_MS`) used by the
//     top-level App shell to tune reconnect cadence, workspace
//     persistence debounce, drag-state staleness, and delayed
//     close behaviour.
//   - The `CREATE_SESSION_WORKSPACE_ID` `"__workspace__"` sentinel
//     used by the new-session picker when the user creates a
//     session without a project (workspace-level scope).
//   - The `NEW_SESSION_AGENT_OPTIONS` label/value list and the
//     `NEW_SESSION_AGENT_OPTIONS_EXHAUSTIVE` compile-time
//     exhaustiveness check that fails the build if a new
//     `AgentType` is added without extending the options list.
//   - App-level record/map helper types
//     (`SessionErrorMap`, `SessionNoticeMap`) and the
//     `StateEventPayload` wrapper that marks SSE state payloads
//     restored from the fallback poll instead of the live stream.
//   - Workspace-layout persistence types
//     (`WorkspaceLayoutPersistencePayload`,
//     `PendingWorkspaceLayoutSave`).
//   - UI-state types for pane/project interactions
//     (`PendingSessionRename`, `OrchestratorRuntimeAction` alias,
//     `SessionConversationItem`, `StandaloneControlSurfaceViewState`).
//
// What this file does NOT own:
//   - The React state or effects that consume these — those stay
//     inside the App component in `App.tsx`.
//   - Agent-specific UI, theme, or workspace tree primitives —
//     those live in their own modules.
//
// Split out of `ui/src/App.tsx`. Every constant value, every type
// shape, and the exhaustiveness-check assertion survive the move
// unchanged; consumers (currently only `App.tsx`) import them
// from here directly.

import type { StateResponse } from "./api";
import type { RuntimeAction } from "./runtime-action-button";
import type { SessionListFilter } from "./session-list-filter";
import type {
  AgentType,
  ExhaustiveValueCoverage,
  Message,
  PendingPrompt,
} from "./types";
import type {
  DiagramLook,
  DiagramPalette,
  DiagramThemeOverrideMode,
  MarkdownStyleId,
  MarkdownThemeId,
  StyleId,
  ThemeId,
} from "./themes";
import type { WorkspaceState } from "./workspace";
import type { ControlPanelSide } from "./workspace-storage";

export const TAB_DRAG_STALE_TIMEOUT_MS = 15000;
export const RECONNECT_STATE_RESYNC_DELAY_MS = 400;
export const RECONNECT_STATE_RESYNC_MAX_DELAY_MS = 5000;
export const LIVE_SESSION_RESUME_WATCHDOG_INTERVAL_MS = 1000;

export const WORKSPACE_LAYOUT_PERSIST_DELAY_MS = 150;

export type SessionErrorMap = Record<string, string | undefined>;
export type StateEventPayload = StateResponse & {
  _sseFallback?: boolean;
};
export type SessionNoticeMap = Record<string, string | undefined>;
export type WorkspaceLayoutPersistencePayload = {
  controlPanelSide: ControlPanelSide;
  densityPercent: number;
  editorFontSizePx: number;
  fontSizePx: number;
  styleId: StyleId;
  themeId: ThemeId;
  markdownStyleId: MarkdownStyleId;
  markdownThemeId: MarkdownThemeId;
  diagramThemeOverrideMode: DiagramThemeOverrideMode;
  diagramLook: DiagramLook;
  diagramPalette: DiagramPalette;
  workspace: WorkspaceState;
};
export type PendingWorkspaceLayoutSave = {
  layout: WorkspaceLayoutPersistencePayload;
  workspaceId: string;
};
export type OrchestratorRuntimeAction = RuntimeAction;
export type PendingSessionRename = {
  clientX: number;
  clientY: number;
  sessionId: string;
};

export const PENDING_KILL_CLOSE_DELAY_MS = 180;
export const PENDING_SESSION_RENAME_CLOSE_DELAY_MS = 300;

export type SessionConversationItem =
  | {
      author: Message["author"];
      id: string;
      kind: "message";
      message: Message;
    }
  | {
      author: "you";
      id: string;
      kind: "pendingPrompt";
      prompt: PendingPrompt;
    };

export const NEW_SESSION_AGENT_OPTIONS = [
  { label: "Claude", value: "Claude" },
  { label: "Codex", value: "Codex" },
  { label: "Cursor", value: "Cursor" },
  { label: "Gemini", value: "Gemini" },
] as const satisfies ReadonlyArray<{ label: string; value: AgentType }>;
// Compile-time exhaustiveness check: adding an `AgentType` variant without
// extending `NEW_SESSION_AGENT_OPTIONS` must fail the build.
export const NEW_SESSION_AGENT_OPTIONS_EXHAUSTIVE: ExhaustiveValueCoverage<
  AgentType,
  typeof NEW_SESSION_AGENT_OPTIONS
> = true;

export const CREATE_SESSION_WORKSPACE_ID = "__workspace__";

export type StandaloneControlSurfaceViewState = {
  projectId?: string;
  sessionListFilter?: SessionListFilter;
  sessionListSearchQuery?: string;
};
