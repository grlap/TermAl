// Owns: rendering and action affordances for parallel-agent message cards.
// Does not own: generic message card dispatch, markdown rendering, or delegation action side effects.
// Split from: ui/src/message-cards.tsx.

import { memo, useCallback, useEffect, useRef, useState } from "react";

import { MessageMeta } from "./message-card-meta";
import { MessageNavigationButtons } from "./panels/conversation-navigation";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import type { ParallelAgentsMessage } from "./types";

export function parallelAgentsHeading(message: ParallelAgentsMessage) {
  const count = message.agents.length;
  const label = count === 1 ? "agent" : "agents";
  const activeCount = message.agents.filter(
    (agent) => agent.status === "initializing" || agent.status === "running",
  ).length;
  if (activeCount > 0) {
    return `Running ${count} ${label}`;
  }

  const errorCount = message.agents.filter(
    (agent) => agent.status === "error",
  ).length;
  if (errorCount > 0) {
    return `${count} ${label} finished with ${errorCount} error${errorCount === 1 ? "" : "s"}`;
  }

  return `${count} ${label} completed`;
}

export function parallelAgentsSummary(message: ParallelAgentsMessage) {
  const activeCount = message.agents.filter(
    (agent) => agent.status === "initializing" || agent.status === "running",
  ).length;
  const completedCount = message.agents.filter(
    (agent) => agent.status === "completed",
  ).length;
  const errorCount = message.agents.filter(
    (agent) => agent.status === "error",
  ).length;

  if (activeCount > 0) {
    const parts = [];
    if (completedCount > 0) {
      parts.push(`${completedCount} done`);
    }
    if (errorCount > 0) {
      parts.push(`${errorCount} failed`);
    }
    parts.push(`${activeCount} active`);
    return parts.join(" \u00b7 ");
  }

  if (errorCount > 0 && completedCount > 0) {
    return `${completedCount} completed \u00b7 ${errorCount} failed`;
  }
  if (errorCount > 0) {
    return `${errorCount} failed`;
  }

  return "All task agents completed.";
}

export function parallelAgentStatusLabel(
  status: ParallelAgentsMessage["agents"][number]["status"],
) {
  switch (status) {
    case "initializing":
      return "initializing";
    case "running":
      return "running";
    case "completed":
      return "completed";
    case "error":
      return "failed";
  }
}

export function parallelAgentStatusTone(
  status: ParallelAgentsMessage["agents"][number]["status"],
) {
  switch (status) {
    case "initializing":
    case "running":
      return "active";
    case "completed":
      return "idle";
    case "error":
      return "error";
  }
}

export function parallelAgentDetail(
  agent: ParallelAgentsMessage["agents"][number],
) {
  if (agent.detail?.trim()) {
    return agent.detail;
  }

  return agent.status === "error" ? "Task failed." : "Initializing...";
}

type ParallelAgent = ParallelAgentsMessage["agents"][number];
type RunParallelAgentAction = (
  actionKey: string,
  action: () => Promise<void> | void,
) => void;

const ParallelAgentRow = memo(function ParallelAgentRow({
  agent,
  isLast,
  pendingActionKeys,
  onOpenAgentSession,
  onInsertAgentResult,
  onCancelAgent,
  runAgentAction,
  searchQuery,
  searchHighlightTone,
}: {
  agent: ParallelAgent;
  isLast: boolean;
  pendingActionKeys: ReadonlySet<string>;
  onOpenAgentSession?: (agentId: string) => Promise<void> | void;
  onInsertAgentResult?: (agentId: string) => Promise<void> | void;
  onCancelAgent?: (agentId: string) => Promise<void> | void;
  runAgentAction: RunParallelAgentAction;
  searchQuery: string;
  searchHighlightTone: SearchHighlightTone;
}) {
  const isDelegationAgent = agent.source === "delegation";
  // Action callbacks receive the bare delegation id because only delegation
  // rows expose actions; tool-source rows are display-only.
  const hasAgentActions =
    isDelegationAgent &&
    (onOpenAgentSession || onInsertAgentResult || onCancelAgent);
  const agentIdentity = `${agent.source}:${agent.id}`;
  const openActionKey = `${agentIdentity}:open`;
  const insertActionKey = `${agentIdentity}:insert`;
  const cancelActionKey = `${agentIdentity}:cancel`;
  const isOpenPending = pendingActionKeys.has(openActionKey);
  const isInsertPending = pendingActionKeys.has(insertActionKey);
  const isCancelPending = pendingActionKeys.has(cancelActionKey);
  const handleOpenAgentSession = useCallback(() => {
    if (!onOpenAgentSession) {
      return;
    }
    runAgentAction(openActionKey, () => onOpenAgentSession(agent.id));
  }, [agent.id, onOpenAgentSession, openActionKey, runAgentAction]);
  const handleInsertAgentResult = useCallback(() => {
    if (!onInsertAgentResult) {
      return;
    }
    runAgentAction(insertActionKey, () => onInsertAgentResult(agent.id));
  }, [agent.id, insertActionKey, onInsertAgentResult, runAgentAction]);
  const handleCancelAgent = useCallback(() => {
    if (!onCancelAgent) {
      return;
    }
    runAgentAction(cancelActionKey, () => onCancelAgent(agent.id));
  }, [agent.id, cancelActionKey, onCancelAgent, runAgentAction]);

  return (
    <li
      className={`parallel-agent-row parallel-agent-row-${parallelAgentStatusTone(agent.status)}`}
    >
      <div className="parallel-agent-line">
        <span className="parallel-agent-branch" aria-hidden="true">
          {isLast ? "\u2514" : "\u251c"}
        </span>
        <div className="parallel-agent-copy">
          <div className="parallel-agent-title-row">
            <span className="parallel-agent-title">
              {renderHighlightedText(
                agent.title,
                searchQuery,
                searchHighlightTone,
              )}
            </span>
            <span
              className={`parallel-agent-status parallel-agent-status-${parallelAgentStatusTone(agent.status)}`}
            >
              {parallelAgentStatusLabel(agent.status)}
            </span>
          </div>
          <div className="parallel-agent-detail-row">
            <span className="parallel-agent-branch-child" aria-hidden="true">
              {isLast ? " " : "\u2502"}
            </span>
            <span className="parallel-agent-detail">
              {renderHighlightedText(
                parallelAgentDetail(agent),
                searchQuery,
                searchHighlightTone,
              )}
            </span>
          </div>
          {hasAgentActions ? (
            <div className="parallel-agent-actions">
              {onOpenAgentSession ? (
                <button
                  className="ghost-button parallel-agent-action"
                  type="button"
                  disabled={isOpenPending}
                  aria-busy={isOpenPending}
                  onClick={handleOpenAgentSession}
                >
                  Open session
                </button>
              ) : null}
              {onInsertAgentResult &&
              (agent.status === "completed" || agent.status === "error") ? (
                <button
                  className="ghost-button parallel-agent-action"
                  type="button"
                  disabled={isInsertPending}
                  aria-busy={isInsertPending}
                  onClick={handleInsertAgentResult}
                >
                  Insert result
                </button>
              ) : null}
              {onCancelAgent &&
              (agent.status === "initializing" ||
                agent.status === "running") ? (
                <button
                  className="ghost-button parallel-agent-action parallel-agent-action-danger"
                  type="button"
                  disabled={isCancelPending}
                  aria-busy={isCancelPending}
                  onClick={handleCancelAgent}
                >
                  Cancel
                </button>
              ) : null}
            </div>
          ) : null}
        </div>
      </div>
    </li>
  );
});

export function ParallelAgentsCard({
  message,
  onOpenAgentSession,
  onInsertAgentResult,
  onCancelAgent,
  actionsEnabled = true,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ParallelAgentsMessage;
  onOpenAgentSession?: (agentId: string) => Promise<void> | void;
  onInsertAgentResult?: (agentId: string) => Promise<void> | void;
  onCancelAgent?: (agentId: string) => Promise<void> | void;
  actionsEnabled?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [expanded, setExpanded] = useState(false);
  const pendingActionKeysRef = useRef<ReadonlySet<string>>(new Set());
  const mountedRef = useRef(true);
  const [pendingActionKeys, setPendingActionKeys] = useState<
    ReadonlySet<string>
  >(pendingActionKeysRef.current);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const hasActiveAgents = message.agents.some(
    (agent) => agent.status === "initializing" || agent.status === "running",
  );
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  useEffect(() => {
    if (hasActiveAgents) {
      setExpanded(true);
    }
  }, [hasActiveAgents]);
  const canCollapse = !hasActiveAgents && !isSearchExpanded;
  const isExpanded = hasActiveAgents || isSearchExpanded || expanded;
  const heading = parallelAgentsHeading(message);
  const summary = parallelAgentsSummary(message);
  const markActionPending = useCallback((actionKey: string) => {
    if (pendingActionKeysRef.current.has(actionKey)) {
      return false;
    }
    const nextKeys = new Set(pendingActionKeysRef.current);
    nextKeys.add(actionKey);
    pendingActionKeysRef.current = nextKeys;
    if (mountedRef.current) {
      setPendingActionKeys(nextKeys);
    }
    return true;
  }, []);
  const clearActionPending = useCallback((actionKey: string) => {
    if (!pendingActionKeysRef.current.has(actionKey)) {
      return;
    }
    const nextKeys = new Set(pendingActionKeysRef.current);
    nextKeys.delete(actionKey);
    pendingActionKeysRef.current = nextKeys;
    if (mountedRef.current) {
      setPendingActionKeys(nextKeys);
    }
  }, []);
  const runAgentAction = useCallback(
    (actionKey: string, action: () => Promise<void> | void) => {
      if (!markActionPending(actionKey)) {
        return;
      }
      let result: Promise<void> | void;
      try {
        result = action();
      } catch (error) {
        clearActionPending(actionKey);
        throw error;
      }
      if (!result || typeof result.then !== "function") {
        clearActionPending(actionKey);
        return;
      }
      void result
        .finally(() => {
          clearActionPending(actionKey);
        })
        .catch(() => {
          // Action handlers own user-facing error reporting; this only prevents
          // the cleanup promise from becoming an unhandled rejection.
        });
    },
    [clearActionPending, markActionPending],
  );

  return (
    <article
      className={`message-card reasoning-card parallel-agents-card${isExpanded ? " is-expanded" : ""}`}
    >
      <MessageMeta
        author={message.author}
        timestamp={message.timestamp}
        trailing={
          <>
            <MessageNavigationButtons
              kind="delegation"
              messageId={message.id}
            />
            {canCollapse ? (
              <button
                className="ghost-button parallel-agents-toggle"
                type="button"
                onClick={() => setExpanded((open) => !open)}
                aria-expanded={isExpanded}
              >
                {isExpanded ? "Hide tasks" : "Show tasks"}
              </button>
            ) : null}
          </>
        }
      />
      <div className="card-label parallel-agents-card-label">
        Parallel agents
      </div>
      <div className="parallel-agents-header">
        <h3>
          {renderHighlightedText(heading, searchQuery, searchHighlightTone)}
        </h3>
        <span className="parallel-agents-summary">{summary}</span>
      </div>
      {isExpanded ? (
        <ol className="parallel-agents-tree">
          {message.agents.map((agent, index) => {
            const agentIdentity = `${agent.source}:${agent.id}`;
            return (
              <ParallelAgentRow
                key={agentIdentity}
                agent={agent}
                isLast={index === message.agents.length - 1}
                pendingActionKeys={pendingActionKeys}
                onOpenAgentSession={
                  actionsEnabled ? onOpenAgentSession : undefined
                }
                onInsertAgentResult={
                  actionsEnabled ? onInsertAgentResult : undefined
                }
                onCancelAgent={actionsEnabled ? onCancelAgent : undefined}
                runAgentAction={runAgentAction}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            );
          })}
        </ol>
      ) : null}
    </article>
  );
}
