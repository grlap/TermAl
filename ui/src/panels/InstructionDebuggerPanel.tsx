import { useEffect, useMemo, useState } from "react";
import { fetchInstructionSearch } from "../api";
import {
  normalizeDisplayPath,
  relativizePathToWorkspace,
} from "../path-display";
import type {
  InstructionDocumentKind,
  InstructionPathStep,
  InstructionRootPath,
  InstructionSearchResponse,
  Session,
} from "../types";

type InstructionDebuggerPanelProps = {
  session: Session | null;
  workdir: string | null;
  onOpenPath: (path: string, options?: { line?: number; openInNewTab?: boolean }) => void;
};

type InstructionDebuggerCacheEntry = {
  error: string | null;
  query: string;
  result: InstructionSearchResponse | null;
};

const instructionDebuggerPanelCache = new Map<string, InstructionDebuggerCacheEntry>();

export function InstructionDebuggerPanel({
  session,
  workdir,
  onOpenPath,
}: InstructionDebuggerPanelProps) {
  const cacheKey = session?.id ?? `workdir:${workdir ?? ""}`;
  const cachedState = instructionDebuggerPanelCache.get(cacheKey) ?? null;
  const [query, setQuery] = useState(cachedState?.query ?? "");
  const [result, setResult] = useState<InstructionSearchResponse | null>(
    cachedState?.result ?? null,
  );
  const [error, setError] = useState<string | null>(cachedState?.error ?? null);
  const [isLoading, setIsLoading] = useState(false);

  useEffect(() => {
    const cached = instructionDebuggerPanelCache.get(cacheKey) ?? null;
    setQuery(cached?.query ?? "");
    setResult(cached?.result ?? null);
    setError(cached?.error ?? null);
    setIsLoading(false);
  }, [cacheKey]);

  const normalizedWorkdir = workdir?.trim() || session?.workdir?.trim() || null;
  const summaryLabel = useMemo(() => {
    if (!result) {
      return null;
    }

    const matchCount = result.matches.length;
    if (matchCount === 0) {
      return "No matches";
    }

    return matchCount === 1 ? "1 match" : `${matchCount} matches`;
  }, [result]);

  async function handleSearch() {
    const trimmedQuery = query.trim();
    if (!session || !trimmedQuery) {
      return;
    }

    setIsLoading(true);
    setError(null);
    try {
      const response = await fetchInstructionSearch(session.id, trimmedQuery);
      setResult(response);
      instructionDebuggerPanelCache.set(cacheKey, {
        error: null,
        query: trimmedQuery,
        result: response,
      });
    } catch (searchError) {
      const message =
        searchError instanceof Error ? searchError.message : "Instruction search failed.";
      setError(message);
      instructionDebuggerPanelCache.set(cacheKey, {
        error: message,
        query: trimmedQuery,
        result: null,
      });
    } finally {
      setIsLoading(false);
    }
  }

  return (
    <div className="source-pane instruction-debugger-panel">
      <div className="source-toolbar instruction-debugger-toolbar">
        <div className="instruction-debugger-toolbar-row">
          <div className="instruction-debugger-toolbar-copy">
            <div className="session-control-label">Instruction debugger</div>
            <p className="support-copy instruction-debugger-support-copy">
              Search instruction files and trace every reachable root path for the phrase.
            </p>
          </div>
          {summaryLabel ? (
            <span className="instruction-debugger-summary-badge">{summaryLabel}</span>
          ) : null}
        </div>
        <form
          className="instruction-debugger-search-row"
          onSubmit={(event) => {
            event.preventDefault();
            void handleSearch();
          }}
        >
          <input
            className="source-path-input instruction-debugger-search-input"
            type="search"
            value={query}
            placeholder="Search instruction phrase, e.g. dependency injection"
            spellCheck={false}
            onChange={(event) => setQuery(event.currentTarget.value)}
          />
          <button
            className="ghost-button instruction-debugger-search-button"
            type="submit"
            disabled={!session || !query.trim() || isLoading}
          >
            {isLoading ? "Searching..." : "Search"}
          </button>
        </form>
        <div className="instruction-debugger-meta-row">
          <span className="instruction-debugger-session-chip">
            {session ? `${session.agent} / ${session.name}` : "No live session"}
          </span>
          {normalizedWorkdir ? (
            <span className="instruction-debugger-workdir" title={normalizedWorkdir}>
              {formatPathLabel(normalizedWorkdir, normalizedWorkdir)}
            </span>
          ) : null}
        </div>
      </div>

      {!session ? (
        <div className="thread-notice">
          <div className="card-label">Instructions</div>
          <p>This tab is no longer attached to a live session.</p>
        </div>
      ) : error ? (
        <div className="thread-notice">
          <div className="card-label">Search error</div>
          <p>{error}</p>
        </div>
      ) : result ? (
        result.matches.length > 0 ? (
          <div className="instruction-debugger-results">
            {result.matches.map((match) => (
              <article
                key={`${match.path}:${match.line}:${match.text}`}
                className="message-card instruction-match-card"
              >
                <header className="instruction-match-header">
                  <div className="instruction-match-copy">
                    <span className="instruction-match-label">Matched file</span>
                    <button
                      className="instruction-debugger-link"
                      type="button"
                      onClick={() =>
                        onOpenPath(match.path, {
                          line: match.line,
                          openInNewTab: true,
                        })
                      }
                    >
                      {formatPathLabel(match.path, normalizedWorkdir)}
                    </button>
                    <span className="instruction-match-line">Line {match.line}</span>
                  </div>
                  <span className="instruction-match-root-count">
                    {formatRootPathSummary(match.rootPaths)}
                  </span>
                </header>
                <p className="instruction-match-text">{match.text}</p>
                <div className="instruction-root-path-list">
                  {match.rootPaths.map((rootPath) => (
                    <InstructionRootPathCard
                      key={`${match.path}:${match.line}:${buildRootPathKey(rootPath)}`}
                      matchLine={match.line}
                      matchPath={match.path}
                      matchText={match.text}
                      workdir={normalizedWorkdir}
                      rootPath={rootPath}
                      onOpenPath={onOpenPath}
                    />
                  ))}
                </div>
              </article>
            ))}
          </div>
        ) : (
          <div className="thread-notice">
            <div className="card-label">No matches</div>
            <p>No discovered instruction lines matched this phrase.</p>
          </div>
        )
      ) : (
        <div className="thread-notice">
          <div className="card-label">Instructions</div>
          <p>Search for a phrase to trace it back through instruction-file roots.</p>
        </div>
      )}
    </div>
  );
}

function InstructionRootPathCard({
  matchLine,
  matchPath,
  matchText,
  rootPath,
  workdir,
  onOpenPath,
}: {
  matchLine: number;
  matchPath: string;
  matchText: string;
  rootPath: InstructionRootPath;
  workdir: string | null;
  onOpenPath: (path: string, options?: { line?: number; openInNewTab?: boolean }) => void;
}) {
  const steps = rootPath.steps;
  const lastStep = steps[steps.length - 1] ?? null;
  const finalPath = lastStep?.toPath ?? rootPath.rootPath;
  const traceItems: Array<
    | {
        kind: "document";
        kindLabel: string;
        line: number | null;
        path: string;
        text: string;
        title: string;
        tone: "root" | "referenced" | "match";
      }
    | {
        kind: "hop";
        hopIndex: number;
        step: InstructionPathStep;
      }
  > = [
    {
      kind: "document",
      kindLabel: formatRootKind(rootPath.rootKind),
      line: steps.length === 0 ? matchLine : null,
      path: rootPath.rootPath,
      text:
        steps.length === 0
          ? matchText
          : "This is the root instruction file where the trace starts.",
      title: steps.length === 0 ? "Matched at root" : "Start from root",
      tone: steps.length === 0 ? "match" : "root",
    },
  ];

  steps.forEach((step, index) => {
    const isTerminalStep = index === steps.length - 1;
    traceItems.push({
      kind: "hop",
      hopIndex: index + 1,
      step,
    });
    traceItems.push({
      kind: "document",
      kindLabel: isTerminalStep ? "Match" : "Referenced",
      line: isTerminalStep ? matchLine : null,
      path: step.toPath,
      text:
        isTerminalStep && step.toPath === matchPath
          ? matchText
          : "This file becomes reachable because of the previous hop.",
      title: isTerminalStep ? "Match found here" : "Reached file",
      tone: isTerminalStep && step.toPath === matchPath ? "match" : "referenced",
    });
  });

  return (
    <section className="instruction-root-path-card">
      <header className="instruction-root-path-header">
        <div className="instruction-root-path-heading">
          <div className="instruction-root-path-copy">
            <span className="instruction-root-kind-badge">
              {formatRootKind(rootPath.rootKind)} root
            </span>
          </div>
          <button
            className="instruction-debugger-link instruction-root-path-link"
            type="button"
            onClick={() =>
              onOpenPath(rootPath.rootPath, {
                openInNewTab: true,
              })
            }
          >
            {formatPathLabel(rootPath.rootPath, workdir)}
          </button>
          <p className="instruction-root-path-summary">
            Follow the trace below to see how this root reaches the matched line.
          </p>
        </div>
        <span className="instruction-root-hop-count">
          {steps.length === 0 ? "Direct root match" : steps.length === 1 ? "1 hop" : `${steps.length} hops`}
        </span>
      </header>
      <div className="instruction-trace-list">
        {traceItems.map((item, index) => (
          <div
            key={
              item.kind === "hop"
                ? `${rootPath.rootPath}:hop:${item.hopIndex}:${item.step.fromPath}:${item.step.line}`
                : `${rootPath.rootPath}:document:${item.path}:${item.title}:${index}`
            }
            className="instruction-trace-item"
          >
            <div className="instruction-trace-index">{index + 1}</div>
            {item.kind === "hop" ? (
              <InstructionTraceHopCard
                hopIndex={item.hopIndex}
                step={item.step}
                workdir={workdir}
                onOpenPath={onOpenPath}
              />
            ) : (
              <InstructionTraceDocumentCard
                kindLabel={item.kindLabel}
                line={item.line}
                path={item.path}
                text={item.text}
                title={item.title}
                tone={item.tone}
                workdir={workdir}
                onOpenPath={onOpenPath}
              />
            )}
          </div>
        ))}
      </div>
      {steps.length === 0 ? (
        <p className="instruction-root-direct-copy">
          The phrase appears directly in this root document.
        </p>
      ) : finalPath !== matchPath ? (
        <p className="instruction-root-direct-copy">
          The traced path ends at {formatPathLabel(finalPath, workdir)} before the matched span.
        </p>
      ) : null}
    </section>
  );
}

function InstructionTraceHopCard({
  hopIndex,
  step,
  workdir,
  onOpenPath,
}: {
  hopIndex: number;
  step: InstructionPathStep;
  workdir: string | null;
  onOpenPath: (path: string, options?: { line?: number; openInNewTab?: boolean }) => void;
}) {
  return (
    <article className="instruction-trace-card instruction-trace-card-hop">
      <div className="instruction-trace-card-top">
        <div className="instruction-trace-card-title">Hop {hopIndex}</div>
        <span className="instruction-root-step-relation">
          {formatRelationLabel(step.relation)}
        </span>
      </div>
      <p className="instruction-trace-description">
        {formatHopSummary(step, workdir)}
      </p>
      <div className="instruction-trace-meta">
        <div className="instruction-trace-meta-group">
          <div className="instruction-trace-meta-label">From</div>
          <button
            className="instruction-debugger-link instruction-trace-path"
            type="button"
            onClick={() =>
              onOpenPath(step.fromPath, {
                line: step.line,
                openInNewTab: true,
              })
            }
          >
            {formatPathLabel(step.fromPath, workdir)}:{step.line}
          </button>
        </div>
        <div className="instruction-trace-meta-group">
          <div className="instruction-trace-meta-label">To</div>
          <div className="instruction-root-step-target">
            {formatPathLabel(step.toPath, workdir)}
          </div>
        </div>
      </div>
      {step.excerpt ? (
        <div className="instruction-trace-evidence">
          <div className="instruction-trace-meta-label">Evidence</div>
          <p className="instruction-root-step-excerpt">{step.excerpt}</p>
        </div>
      ) : null}
    </article>
  );
}

function InstructionTraceDocumentCard({
  kindLabel,
  line,
  path,
  text,
  title,
  tone,
  workdir,
  onOpenPath,
}: {
  kindLabel: string;
  line: number | null;
  path: string;
  text: string;
  title: string;
  tone: "root" | "referenced" | "match";
  workdir: string | null;
  onOpenPath: (path: string, options?: { line?: number; openInNewTab?: boolean }) => void;
}) {
  return (
    <article className={`instruction-trace-card instruction-trace-card-${tone}`}>
      <div className="instruction-trace-card-top">
        <div className="instruction-trace-card-title">{title}</div>
        <span className="instruction-trace-card-kind">{kindLabel}</span>
        {line ? <div className="instruction-node-card-line">Line {line}</div> : null}
      </div>
      <header className="instruction-node-card-header">
        <button
          className="instruction-debugger-link instruction-trace-path"
          type="button"
          onClick={() =>
            onOpenPath(path, {
              line: line ?? undefined,
              openInNewTab: true,
            })
          }
        >
          {formatPathLabel(path, workdir)}
        </button>
      </header>
      <p className="instruction-trace-description">{text}</p>
    </article>
  );
}

function formatPathLabel(path: string, workdir: string | null) {
  const relative = relativizePathToWorkspace(path, workdir);
  return normalizeDisplayPath(relative || path);
}

function formatRootKind(kind: InstructionDocumentKind) {
  switch (kind) {
    case "rootInstruction":
      return "Root";
    case "commandInstruction":
      return "Command";
    case "reviewerInstruction":
      return "Reviewer";
    case "rulesInstruction":
      return "Rules";
    case "skillInstruction":
      return "Skill";
    case "referencedInstruction":
      return "Referenced";
    default:
      return "Instruction";
  }
}

function formatRelationLabel(relation: InstructionPathStep["relation"]) {
  switch (relation) {
    case "markdownLink":
      return "linked";
    case "fileReference":
      return "referenced";
    case "directoryDiscovery":
      return "discovered";
    default:
      return "reached";
  }
}

function formatHopSummary(step: InstructionPathStep, workdir: string | null) {
  return `${formatPathLabel(step.fromPath, workdir)}:${step.line} ${formatRelationVerb(step.relation)} ${formatPathLabel(step.toPath, workdir)}.`;
}

function formatRelationVerb(relation: InstructionPathStep["relation"]) {
  switch (relation) {
    case "markdownLink":
      return "links to";
    case "fileReference":
      return "references";
    case "directoryDiscovery":
      return "discovers";
    default:
      return "reaches";
  }
}

function buildRootPathKey(rootPath: InstructionRootPath) {
  const serializedSteps = rootPath.steps
    .map(
      (step) =>
        `${step.fromPath}:${step.line}:${step.relation}:${step.toPath}:${step.excerpt}`,
    )
    .join("|");
  return `${rootPath.rootPath}:${rootPath.rootKind}:${serializedSteps}`;
}

function formatRootPathSummary(rootPaths: InstructionRootPath[]) {
  const uniqueRootCount = new Set(rootPaths.map((rootPath) => rootPath.rootPath)).size;
  const pathCount = rootPaths.length;

  if (pathCount === 0) {
    return "0 roots";
  }

  if (uniqueRootCount === pathCount) {
    return pathCount === 1 ? "1 root" : `${pathCount} roots`;
  }

  const rootLabel = uniqueRootCount === 1 ? "1 root" : `${uniqueRootCount} roots`;
  const pathLabel = pathCount === 1 ? "1 path" : `${pathCount} paths`;
  return `${rootLabel}, ${pathLabel}`;
}
