// Owns: rendering file-change summary message cards.
// Does not own: generic MessageCard dispatch, markdown rendering, or source-file opening side effects.
// Split from: ui/src/message-cards.tsx.

import {
  useEffect,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";

import { copyTextToClipboard } from "./clipboard";
import {
  CheckIcon,
  CollapseIcon,
  CopyIcon,
  ExpandIcon,
  PreviewIcon,
} from "./message-card-icons";
import { MessageMeta } from "./message-card-meta";
import type { MarkdownFileLinkTarget } from "./markdown-links";
import { relativizePathToWorkspace } from "./path-display";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import type { FileChangesMessage } from "./types";

const FILE_CHANGES_COLLAPSE_THRESHOLD = 6;

export function FileChangesCard({
  message,
  onOpenSourceLink,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: FileChangesMessage;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [copiedPath, setCopiedPath] = useState<string | null>(null);
  const [filesExpanded, setFilesExpanded] = useState(false);
  const canExpandFiles = message.files.length > FILE_CHANGES_COLLAPSE_THRESHOLD;
  const isSearchExpanded = searchQuery.trim().length > 0;
  const isFilesExpanded = !canExpandFiles || filesExpanded || isSearchExpanded;
  const collapseControlLabel = filesExpanded
    ? "Collapse changed files"
    : "Expand changed files";

  useEffect(() => {
    if (!copiedPath) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopiedPath(null);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copiedPath]);

  async function handleCopyPath(path: string) {
    try {
      await copyTextToClipboard(path);
      setCopiedPath(path);
    } catch {
      setCopiedPath(null);
    }
  }

  function handleOpenPath(
    path: string,
    event: ReactMouseEvent<HTMLButtonElement>,
  ) {
    onOpenSourceLink?.({
      path,
      openInNewTab: event.ctrlKey || event.metaKey,
    });
  }

  return (
    <article className="message-card utility-card file-changes-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Files</div>
      <div className="command-panel file-changes-panel">
        <div className="command-row file-changes-summary-row">
          <div className="command-row-label">TURN</div>
          <div className="command-row-body">
            <p className="file-changes-title">
              {renderHighlightedText(
                message.title,
                searchQuery,
                searchHighlightTone,
              )}
            </p>
          </div>
          {canExpandFiles && !isSearchExpanded ? (
            <div className="command-row-actions">
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setFilesExpanded((open) => !open)}
                aria-label={collapseControlLabel}
                aria-expanded={filesExpanded}
                title={collapseControlLabel}
              >
                {isFilesExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            </div>
          ) : null}
        </div>
        {isFilesExpanded
          ? message.files.map((file) => {
              const displayPath = relativizePathToWorkspace(
                file.path,
                workspaceRoot,
              );
              const copied = copiedPath === file.path;

              return (
                <div
                  className="command-row file-change-row"
                  key={`${file.kind}:${file.path}`}
                >
                  <div className="command-row-label">
                    <span
                      className={`file-change-kind file-change-kind-${file.kind}`}
                    >
                      {fileChangeKindLabel(file.kind)}
                    </span>
                  </div>
                  <div className="command-row-body">
                    <div
                      className="file-change-path"
                      title={displayPath !== file.path ? file.path : undefined}
                    >
                      {renderHighlightedText(
                        displayPath,
                        searchQuery,
                        searchHighlightTone,
                      )}
                    </div>
                  </div>
                  <div className="command-row-actions">
                    <button
                      className="command-icon-button"
                      type="button"
                      onClick={(event) => handleOpenPath(file.path, event)}
                      disabled={!onOpenSourceLink}
                      aria-label={`Open ${displayPath}`}
                      title="Open file"
                    >
                      <PreviewIcon />
                    </button>
                    <button
                      className={`command-icon-button${copied ? " copied" : ""}`}
                      type="button"
                      onClick={() => void handleCopyPath(file.path)}
                      aria-label={
                        copied ? "Path copied" : `Copy ${displayPath}`
                      }
                      title={copied ? "Copied" : "Copy path"}
                    >
                      {copied ? <CheckIcon /> : <CopyIcon />}
                    </button>
                  </div>
                </div>
              );
            })
          : null}
      </div>
    </article>
  );
}

export function fileChangeKindLabel(
  kind: FileChangesMessage["files"][number]["kind"],
) {
  switch (kind) {
    case "created":
      return "A";
    case "modified":
      return "M";
    case "deleted":
      return "D";
    case "other":
      return "*";
  }
}
