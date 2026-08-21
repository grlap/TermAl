// Owns migration of saved pane/session scroll state when a workspace tab moves.
// Does not own DOM scrolling, workspace placement, or first-visit defaults.
// Split from: ui/src/App.tsx and ui/src/SessionPaneView.scroll.ts.

import { resolveSessionPaneScrollStateKey } from "./SessionPaneView.scroll-key";
import type { SessionPaneViewMode } from "./workspace";

const SESSION_SCROLL_VIEW_MODES: readonly SessionPaneViewMode[] = [
  "session",
  "prompt",
  "commands",
  "diffs",
];

export type PaneScrollPosition = {
  shouldStick: boolean;
  top: number;
};

export type PaneScrollPositionsByPane = Record<
  string,
  Record<string, PaneScrollPosition>
>;

export type PaneScrollPositionMigration = {
  rollback: () => void;
};

export function beginSessionPaneScrollPositionMigration({
  paneShouldStickToBottom,
  paneScrollPositions,
  sessionId,
  sourcePaneId,
  targetPaneId,
}: {
  paneShouldStickToBottom: Record<string, boolean | undefined>;
  paneScrollPositions: PaneScrollPositionsByPane;
  sessionId: string;
  sourcePaneId: string;
  targetPaneId: string;
}): PaneScrollPositionMigration | null {
  if (sourcePaneId === targetPaneId) {
    return null;
  }
  const sourcePositions = paneScrollPositions[sourcePaneId];
  const targetPositionsExisted = Object.prototype.hasOwnProperty.call(
    paneScrollPositions,
    targetPaneId,
  );
  const snapshots: Array<{
    sourceKey: string;
    sourcePosition: PaneScrollPosition | undefined;
    sourcePositionExisted: boolean;
    sourceStickyIntent: boolean | undefined;
    sourceStickyIntentExisted: boolean;
    targetKey: string;
    targetPosition: PaneScrollPosition | undefined;
    targetPositionExisted: boolean;
    targetStickyIntent: boolean | undefined;
    targetStickyIntentExisted: boolean;
  }> = [];

  for (const viewMode of SESSION_SCROLL_VIEW_MODES) {
    const sourceKey = resolveSessionPaneScrollStateKey(
      sourcePaneId,
      viewMode,
      sessionId,
      null,
    );
    const targetKey = resolveSessionPaneScrollStateKey(
      targetPaneId,
      viewMode,
      sessionId,
      null,
    );
    const savedPosition = sourcePositions?.[sourceKey];
    const sourcePositionExisted = Object.prototype.hasOwnProperty.call(
      sourcePositions ?? {},
      sourceKey,
    );
    const hasStickyIntent = Object.prototype.hasOwnProperty.call(
      paneShouldStickToBottom,
      sourceKey,
    );
    if (!sourcePositionExisted && !hasStickyIntent) {
      continue;
    }
    const targetPositions = paneScrollPositions[targetPaneId];
    snapshots.push({
      sourceKey,
      sourcePosition: savedPosition,
      sourcePositionExisted,
      sourceStickyIntent: paneShouldStickToBottom[sourceKey],
      sourceStickyIntentExisted: hasStickyIntent,
      targetKey,
      targetPosition: targetPositions?.[targetKey],
      targetPositionExisted: Object.prototype.hasOwnProperty.call(
        targetPositions ?? {},
        targetKey,
      ),
      targetStickyIntent: paneShouldStickToBottom[targetKey],
      targetStickyIntentExisted: Object.prototype.hasOwnProperty.call(
        paneShouldStickToBottom,
        targetKey,
      ),
    });

    if (sourcePositionExisted && savedPosition) {
      const writableTargetPositions =
        paneScrollPositions[targetPaneId] ??
        (paneScrollPositions[targetPaneId] = {});
      writableTargetPositions[targetKey] = savedPosition;
      delete sourcePositions[sourceKey];
    }
    if (hasStickyIntent) {
      paneShouldStickToBottom[targetKey] =
        paneShouldStickToBottom[sourceKey];
      delete paneShouldStickToBottom[sourceKey];
    }
  }

  if (snapshots.length === 0) {
    return null;
  }

  return {
    rollback: () => {
      for (const snapshot of snapshots) {
        if (snapshot.sourcePositionExisted && snapshot.sourcePosition) {
          const writableSourcePositions =
            paneScrollPositions[sourcePaneId] ??
            (paneScrollPositions[sourcePaneId] = {});
          writableSourcePositions[snapshot.sourceKey] =
            snapshot.sourcePosition;
        } else {
          delete paneScrollPositions[sourcePaneId]?.[snapshot.sourceKey];
        }
        if (snapshot.targetPositionExisted && snapshot.targetPosition) {
          const writableTargetPositions =
            paneScrollPositions[targetPaneId] ??
            (paneScrollPositions[targetPaneId] = {});
          writableTargetPositions[snapshot.targetKey] =
            snapshot.targetPosition;
        } else {
          delete paneScrollPositions[targetPaneId]?.[snapshot.targetKey];
        }
        if (snapshot.sourceStickyIntentExisted) {
          paneShouldStickToBottom[snapshot.sourceKey] =
            snapshot.sourceStickyIntent;
        } else {
          delete paneShouldStickToBottom[snapshot.sourceKey];
        }
        if (snapshot.targetStickyIntentExisted) {
          paneShouldStickToBottom[snapshot.targetKey] =
            snapshot.targetStickyIntent;
        } else {
          delete paneShouldStickToBottom[snapshot.targetKey];
        }
      }
      if (
        !targetPositionsExisted &&
        Object.keys(paneScrollPositions[targetPaneId] ?? {}).length === 0
      ) {
        delete paneScrollPositions[targetPaneId];
      }
    },
  };
}

export function migrateSessionPaneScrollPosition(
  input: Parameters<typeof beginSessionPaneScrollPositionMigration>[0],
): boolean {
  return beginSessionPaneScrollPositionMigration(input) !== null;
}
