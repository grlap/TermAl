import { useEffect, useMemo, useState } from "react";
import { copyTextToClipboard } from "./clipboard";
import { DeferredHighlightedCodeBlock } from "./highlighted-code-block";
import {
  CheckIcon,
  CollapseIcon,
  CopyIcon,
  ExpandIcon,
  PreviewIcon,
} from "./message-card-icons";
import { MessageMeta } from "./message-card-meta";
import { buildDiffPreviewModel } from "./diff-preview";
import { relativizePathToWorkspace } from "./path-display";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import type { DiffMessage } from "./types";

export function DiffCard({
  message,
  onOpenPreview,
  preferImmediateHeavyRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  message: DiffMessage;
  onOpenPreview: () => void;
  preferImmediateHeavyRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const diffStats = useMemo(() => {
    const changeSummary = buildDiffPreviewModel(
      message.diff,
      message.changeType,
    ).changeSummary;
    return {
      addedLineCount:
        changeSummary.changedLineCount + changeSummary.addedLineCount,
      removedLineCount:
        changeSummary.changedLineCount + changeSummary.removedLineCount,
    };
  }, [message.changeType, message.diff]);
  const displayPath = useMemo(
    () => relativizePathToWorkspace(message.filePath, workspaceRoot),
    [message.filePath, workspaceRoot],
  );
  const canExpandDiff =
    message.diff.split("\n").length > 14 || message.diff.length > 900;
  const isExpanded =
    !canExpandDiff || expanded || searchQuery.trim().length > 0;

  useEffect(() => {
    if (!copied) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopied(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copied]);

  async function handleCopy() {
    try {
      await copyTextToClipboard(message.diff);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <article className="message-card utility-card diff-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">
        {message.changeType === "create" ? "New file" : "File edit"}
      </div>
      <div className="command-panel diff-panel">
        <div className="command-row diff-file-row">
          <div className="command-row-label diff-file-label">
            <span>FILE</span>
            {diffStats.addedLineCount > 0 || diffStats.removedLineCount > 0 ? (
              <div className="diff-file-stats">
                {diffStats.addedLineCount > 0 ? (
                  <span className="diff-preview-stat diff-preview-stat-added">
                    +{diffStats.addedLineCount}
                  </span>
                ) : null}
                {diffStats.removedLineCount > 0 ? (
                  <span className="diff-preview-stat diff-preview-stat-removed">
                    -{diffStats.removedLineCount}
                  </span>
                ) : null}
              </div>
            ) : null}
          </div>
          <div className="command-row-body">
            <div
              className="diff-file-path"
              title={
                displayPath !== message.filePath ? message.filePath : undefined
              }
            >
              {renderHighlightedText(
                displayPath,
                searchQuery,
                searchHighlightTone,
              )}
            </div>
            <p className="diff-file-summary">
              {renderHighlightedText(
                message.summary,
                searchQuery,
                searchHighlightTone,
              )}
            </p>
          </div>
        </div>
        <div className="command-row diff-row">
          <div className="command-row-label">DIFF</div>
          <div className="command-row-body">
            <div
              className={`diff-preview-shell ${isExpanded ? "expanded" : "collapsed"}`}
            >
              <DeferredHighlightedCodeBlock
                className="diff-block diff-preview-text"
                code={message.diff}
                language={message.language ?? "diff"}
                pathHint={message.filePath}
                preferImmediateRender={preferImmediateHeavyRender}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className="command-icon-button"
              type="button"
              onClick={onOpenPreview}
              aria-label="Open diff preview"
              title="Open diff preview"
            >
              <PreviewIcon />
            </button>
            <button
              className={`command-icon-button${copied ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy()}
              aria-label={copied ? "Diff copied" : "Copy diff"}
              title={copied ? "Copied" : "Copy diff"}
            >
              {copied ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandDiff ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setExpanded((open) => !open)}
                aria-label={isExpanded ? "Collapse diff" : "Expand diff"}
                aria-pressed={isExpanded}
                title={isExpanded ? "Collapse diff" : "Expand diff"}
              >
                {isExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>
      </div>
    </article>
  );
}
