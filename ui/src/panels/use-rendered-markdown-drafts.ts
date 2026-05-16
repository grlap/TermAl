// Owns rendered-Markdown draft registration and commit application for SourcePanel.
// Does not own file loading/saving, Monaco editor state, or preview rendering.
// Split from `ui/src/panels/SourcePanel.tsx`.

import { useCallback, useLayoutEffect, useRef, useState } from "react";

import type { MonacoCodeEditorStatus } from "../MonacoCodeEditor";
import type { SourceFileState } from "./SourcePanel";
import {
  hasOverlappingMarkdownCommitRanges,
  resolveRenderedMarkdownCommitRange,
  type MarkdownDocumentRange,
  type RenderedMarkdownSectionCommit,
} from "./markdown-commit-ranges";
import {
  applyMarkdownDocumentEolStyle,
  detectMarkdownDocumentEolStyle,
  normalizeEditedMarkdownSection,
  normalizeMarkdownDocumentLineEndings,
  replaceMarkdownDocumentRange,
  type MarkdownDiffDocumentSegment,
} from "./markdown-diff-segments";

type MutableRef<T> = {
  current: T;
};

export function useRenderedMarkdownDrafts({
  createEditorStatusSnapshot,
  editorValueRef,
  fileState,
  fileStateRef,
  isMarkdownSource,
  setActionError,
  setEditorStatus,
  setEditorValueState,
  setSaveConflictOnDisk,
}: {
  createEditorStatusSnapshot: (content: string) => MonacoCodeEditorStatus;
  editorValueRef: MutableRef<string>;
  fileState: SourceFileState;
  fileStateRef: MutableRef<SourceFileState>;
  isMarkdownSource: boolean;
  setActionError: (nextError: string | null) => void;
  setEditorStatus: (nextStatus: MonacoCodeEditorStatus) => void;
  setEditorValueState: (nextValue: string) => void;
  setSaveConflictOnDisk: (nextValue: boolean) => void;
}) {
  const [hasRenderedMarkdownDraftActive, setRenderedMarkdownDraftActive] =
    useState(false);
  const renderedMarkdownDocumentPathRef = useRef(
    fileState.status === "ready" ? fileState.path : null,
  );
  const renderedMarkdownCommittersRef = useRef(
    new Set<() => RenderedMarkdownSectionCommit | null>(),
  );

  useLayoutEffect(() => {
    const nextRenderedMarkdownDocumentPath =
      fileState.status === "ready" ? fileState.path : null;
    if (renderedMarkdownDocumentPathRef.current !== nextRenderedMarkdownDocumentPath) {
      renderedMarkdownDocumentPathRef.current = nextRenderedMarkdownDocumentPath;
      renderedMarkdownCommittersRef.current.clear();
    }
  }, [fileState.path, fileState.status]);

  const registerRenderedMarkdownCommitter = useCallback(
    (committer: () => RenderedMarkdownSectionCommit | null) => {
      renderedMarkdownCommittersRef.current.add(committer);
      return () => {
        renderedMarkdownCommittersRef.current.delete(committer);
      };
    },
    [],
  );

  const collectRenderedMarkdownCommits = useCallback(() => {
    return Array.from(renderedMarkdownCommittersRef.current)
      .map((committer) => committer())
      .filter((commit): commit is RenderedMarkdownSectionCommit => commit != null);
  }, []);

  const handleRenderedMarkdownSectionCommits = useCallback(
    (commits: RenderedMarkdownSectionCommit[]) => {
      const currentFileState = fileStateRef.current;
      if (!isMarkdownSource || currentFileState.status !== "ready") {
        return false;
      }

      const rawSourceContent = editorValueRef.current;
      const originalEolStyle = detectMarkdownDocumentEolStyle(rawSourceContent);
      const sourceContent = normalizeMarkdownDocumentLineEndings(rawSourceContent);
      const resolvedCommits = commits.map((commit) => ({
        commit,
        range: resolveRenderedMarkdownCommitRange(sourceContent, commit),
      }));
      const unresolvedCommitCount = resolvedCommits.filter(
        (entry) => entry.range === null,
      ).length;
      const validResolvedCommits = resolvedCommits.filter(
        (entry): entry is {
          commit: RenderedMarkdownSectionCommit;
          range: MarkdownDocumentRange;
        } => entry.range !== null,
      );
      const hasOverlappingRange =
        hasOverlappingMarkdownCommitRanges(validResolvedCommits);
      if (unresolvedCommitCount > 0 || hasOverlappingRange) {
        setActionError(
          "Rendered Markdown edit could not be applied because the document changed under that section. Review the latest file and edit again.",
        );
        return false;
      }

      const nextDocumentContentLf = validResolvedCommits
        .sort((left, right) => right.range.start - left.range.start)
        .reduce(
          (currentContent, { commit, range }) =>
            replaceMarkdownDocumentRange(
              currentContent,
              range.start,
              range.end,
              normalizeEditedMarkdownSection(
                commit.nextMarkdown,
                commit.segment.markdown,
              ),
            ),
          sourceContent,
        );
      if (nextDocumentContentLf === sourceContent) {
        commits.forEach((commit) =>
          commit.onApplied?.({ resetRenderedContent: false }),
        );
        setRenderedMarkdownDraftActive(false);
        return true;
      }

      const nextDocumentContent = applyMarkdownDocumentEolStyle(
        nextDocumentContentLf,
        originalEolStyle,
      );
      setRenderedMarkdownDraftActive(false);
      setEditorValueState(nextDocumentContent);
      setEditorStatus(createEditorStatusSnapshot(nextDocumentContent));
      setActionError(null);
      setSaveConflictOnDisk(false);
      commits.forEach((commit) => commit.onApplied?.());
      return true;
    },
    [
      createEditorStatusSnapshot,
      editorValueRef,
      fileStateRef,
      isMarkdownSource,
      setActionError,
      setEditorStatus,
      setEditorValueState,
      setSaveConflictOnDisk,
    ],
  );

  const commitRenderedMarkdownDrafts = useCallback((): boolean => {
    const commits = collectRenderedMarkdownCommits();
    if (commits.length === 0) {
      setRenderedMarkdownDraftActive(false);
      return true;
    }

    return handleRenderedMarkdownSectionCommits(commits);
  }, [collectRenderedMarkdownCommits, handleRenderedMarkdownSectionCommits]);

  const commitRenderedMarkdownSectionDraft = useCallback(
    (commit: RenderedMarkdownSectionCommit) => {
      return handleRenderedMarkdownSectionCommits([commit]);
    },
    [handleRenderedMarkdownSectionCommits],
  );

  const handleRenderedMarkdownSectionDraftChange = useCallback(
    (segment: MarkdownDiffDocumentSegment, nextMarkdown: string) => {
      if (!isMarkdownSource || fileStateRef.current.status !== "ready") {
        return;
      }

      const normalizedDraft = normalizeEditedMarkdownSection(
        nextMarkdown,
        segment.markdown,
      );
      const nextHasDraft = normalizedDraft !== segment.markdown;
      setRenderedMarkdownDraftActive(nextHasDraft);
      setActionError(null);
      setSaveConflictOnDisk(false);
    },
    [fileStateRef, isMarkdownSource, setActionError, setSaveConflictOnDisk],
  );

  const handleRenderedMarkdownReadOnlyMutation = useCallback(() => {
    // SourcePanel Markdown preview is editable; this is only required by
    // the shared rendered-Markdown section API.
  }, []);

  return {
    commitRenderedMarkdownDrafts,
    commitRenderedMarkdownSectionDraft,
    handleRenderedMarkdownReadOnlyMutation,
    handleRenderedMarkdownSectionDraftChange,
    hasRenderedMarkdownDraftActive,
    registerRenderedMarkdownCommitter,
    setRenderedMarkdownDraftActive,
  };
}
