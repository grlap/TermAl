import { type FormEvent, useEffect, useMemo, useRef, useState } from "react";

import { runTerminalCommand, type TerminalCommandResponse } from "../api";
import { getErrorMessage } from "../app-utils";

type TerminalHistoryEntry = {
  command: string;
  error: string | null;
  id: string;
  response: TerminalCommandResponse | null;
  startedAt: string;
  status: "running" | "done" | "error";
  workdir: string;
};

const terminalHistoryById = new Map<string, TerminalHistoryEntry[]>();

export function TerminalPanel({
  projectId = null,
  sessionId = null,
  showPathControls = true,
  terminalId,
  workdir,
  onOpenWorkdir,
}: {
  projectId?: string | null;
  sessionId?: string | null;
  showPathControls?: boolean;
  terminalId: string;
  workdir: string | null;
  onOpenWorkdir: (path: string) => void;
}) {
  const normalizedWorkdir = workdir?.trim() ?? "";
  const normalizedProjectId = projectId?.trim() ?? "";
  const normalizedSessionId = sessionId?.trim() ?? "";
  const hasScope = Boolean(normalizedProjectId || normalizedSessionId);
  const [commandDraft, setCommandDraft] = useState("");
  const [workdirDraft, setWorkdirDraft] = useState(normalizedWorkdir);
  const [history, setHistory] = useState<TerminalHistoryEntry[]>(
    () =>
      (terminalHistoryById.get(terminalId) ?? []).map((entry) =>
        entry.status === "running"
          ? { ...entry, status: "error" as const, error: "Interrupted" }
          : entry,
      ),
  );
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const isRunning = history.some((entry) => entry.status === "running");
  const canRun = Boolean(commandDraft.trim() && normalizedWorkdir && hasScope && !isRunning);
  const terminalLabel = useMemo(
    () => formatTerminalWorkdirLabel(normalizedWorkdir),
    [normalizedWorkdir],
  );

  useEffect(() => {
    setWorkdirDraft(normalizedWorkdir);
  }, [normalizedWorkdir]);

  useEffect(() => {
    terminalHistoryById.set(terminalId, history);
  }, [history, terminalId]);

  useEffect(() => {
    const node = scrollRef.current;
    if (!node) {
      return;
    }
    node.scrollTop = node.scrollHeight;
  }, [history]);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const command = commandDraft.trim();
    if (!command || !normalizedWorkdir || !hasScope || isRunning) {
      return;
    }

    const entryId = crypto.randomUUID();
    const startedAt = new Date().toLocaleTimeString();
    const entry: TerminalHistoryEntry = {
      command,
      error: null,
      id: entryId,
      response: null,
      startedAt,
      status: "running",
      workdir: normalizedWorkdir,
    };
    setHistory((current) => [...current, entry]);
    setCommandDraft("");

    try {
      const response = await runTerminalCommand({
        command,
        projectId: normalizedProjectId || null,
        sessionId: normalizedSessionId || null,
        workdir: normalizedWorkdir,
      });
      const updater = (item: TerminalHistoryEntry) =>
        item.id === entryId
          ? {
              ...item,
              response,
              status: "done" as const,
              workdir: response.workdir,
            }
          : item;
      setHistory((current) => current.map(updater));
      const cached = terminalHistoryById.get(terminalId);
      if (cached) {
        terminalHistoryById.set(terminalId, cached.map(updater));
      }
    } catch (error) {
      const updater = (item: TerminalHistoryEntry) =>
        item.id === entryId
          ? {
              ...item,
              error: getErrorMessage(error),
              status: "error" as const,
            }
          : item;
      setHistory((current) => current.map(updater));
      const cached = terminalHistoryById.get(terminalId);
      if (cached) {
        terminalHistoryById.set(terminalId, cached.map(updater));
      }
    }
  }

  function clearHistory() {
    setHistory([]);
    terminalHistoryById.delete(terminalId);
  }

  return (
    <section className="terminal-panel" aria-label="Terminal">
      <div className="terminal-toolbar">
        <div className="terminal-title-group">
          <span className="card-label">Terminal</span>
          <strong title={normalizedWorkdir || undefined}>{terminalLabel}</strong>
        </div>
        <button
          className="ghost-button"
          type="button"
          onClick={clearHistory}
          disabled={history.length === 0 || isRunning}
        >
          Clear
        </button>
      </div>

      {showPathControls ? (
        <form
          className="terminal-workdir-row"
          onSubmit={(event) => {
            event.preventDefault();
            const nextWorkdir = workdirDraft.trim();
            if (nextWorkdir) {
              onOpenWorkdir(nextWorkdir);
            }
          }}
        >
          <label className="terminal-workdir-label" htmlFor={`terminal-workdir-${terminalId}`}>
            Working directory
          </label>
          <input
            id={`terminal-workdir-${terminalId}`}
            className="terminal-workdir-input"
            value={workdirDraft}
            onChange={(event) => setWorkdirDraft(event.target.value)}
            spellCheck={false}
          />
          <button className="ghost-button terminal-workdir-button" type="submit">
            Use
          </button>
        </form>
      ) : null}

      {!hasScope ? (
        <div className="terminal-notice" role="alert">
          This terminal is no longer associated with a live session or project.
        </div>
      ) : null}

      <div ref={scrollRef} className="terminal-history" role="log" aria-live="polite">
        {history.length === 0 ? (
          <div className="terminal-empty-state">
            Run a command in this workspace.
          </div>
        ) : (
          history.map((entry) => <TerminalHistoryItem key={entry.id} entry={entry} />)
        )}
      </div>

      <form className="terminal-command-row" onSubmit={handleSubmit}>
        <span className="terminal-prompt" aria-hidden="true">
          $
        </span>
        <input
          className="terminal-command-input"
          value={commandDraft}
          onChange={(event) => setCommandDraft(event.target.value)}
          placeholder={normalizedWorkdir ? "Command" : "Set a working directory first"}
          spellCheck={false}
          disabled={!normalizedWorkdir || !hasScope || isRunning}
          aria-label="Terminal command"
        />
        <button className="send-button terminal-run-button" type="submit" disabled={!canRun}>
          {isRunning ? "Running" : "Run"}
        </button>
      </form>
    </section>
  );
}

function TerminalHistoryItem({ entry }: { entry: TerminalHistoryEntry }) {
  const response = entry.response;
  const statusLabel =
    entry.status === "running"
      ? "Running"
      : entry.status === "error"
        ? "Failed"
        : formatTerminalResult(response);

  return (
    <article className={`terminal-history-item is-${entry.status}`}>
      <header className="terminal-history-header">
        <span className="terminal-command-line">
          <span className="terminal-prompt" aria-hidden="true">
            $
          </span>
          <code>{entry.command}</code>
        </span>
        <span className="terminal-history-meta">
          {entry.startedAt} - {statusLabel}
        </span>
      </header>
      <div className="terminal-history-workdir" title={entry.workdir}>
        {entry.workdir}
      </div>
      {entry.error ? (
        <pre className="terminal-output terminal-output-error">{entry.error}</pre>
      ) : null}
      {response?.stdout ? (
        <pre className="terminal-output terminal-output-stdout">{response.stdout}</pre>
      ) : null}
      {response?.stderr ? (
        <pre className="terminal-output terminal-output-stderr">{response.stderr}</pre>
      ) : null}
      {response?.outputTruncated ? (
        <div className="terminal-output-note">Output was truncated.</div>
      ) : null}
      {entry.status === "running" ? (
        <div className="terminal-output-note">Waiting for command output...</div>
      ) : null}
    </article>
  );
}

export function formatTerminalResult(response: TerminalCommandResponse | null) {
  if (!response) {
    return "Done";
  }
  if (response.timedOut) {
    return `Timed out after ${formatDuration(response.durationMs)}`;
  }

  const code = response.exitCode ?? "signal";
  return `Exit ${code} in ${formatDuration(response.durationMs)}`;
}

function formatDuration(durationMs: number) {
  if (durationMs < 1000) {
    return `${durationMs}ms`;
  }

  return `${(durationMs / 1000).toFixed(1)}s`;
}

export function formatTerminalWorkdirLabel(workdir: string) {
  if (!workdir) {
    return "No working directory";
  }

  const segments = workdir.split(/[/\\]+/).filter(Boolean);
  return segments[segments.length - 1] ?? workdir;
}
