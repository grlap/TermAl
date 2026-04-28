import {
  type FormEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";

import {
  ApiRequestError,
  runTerminalCommandStream,
  type TerminalCommandOutputEvent,
  type TerminalCommandResponse,
} from "../api";
import { getErrorMessage } from "../app-utils";
import { assertNever } from "../exhaustive";

export type TerminalHistoryEntry = {
  command: string;
  error: string | null;
  id: string;
  response: TerminalCommandResponse | null;
  startedAt: string;
  status: "running" | "done" | "error";
  stderr?: string;
  stdout?: string;
  workdir: string;
};

type TerminalHistoryStore = {
  history: TerminalHistoryEntry[];
  listeners: Set<() => void>;
};

const terminalHistoryById = new Map<string, TerminalHistoryStore>();
const inFlightTerminalEntries = new Set<string>();
const emptyTerminalHistory: readonly TerminalHistoryEntry[] = [];
let liveTerminalIds: Set<string> | null = null;

function getOrCreateTerminalHistoryStore(terminalId: string) {
  let store = terminalHistoryById.get(terminalId);
  if (!store) {
    // Initialize with the shared `emptyTerminalHistory` constant so the
    // post-subscribe snapshot keeps the same referential identity as the
    // render-time snapshot returned by `getTerminalHistorySnapshot` when
    // the store did not yet exist. Without this, React's tearing check
    // would see two distinct empty arrays via `Object.is` and schedule a
    // wasted re-render on every first-mount of a TerminalPanel. The cast
    // is safe because both `reconcileStaleRunningEntries` and
    // `setTerminalHistory` replace `store.history` with a freshly-built
    // array instead of mutating in place.
    store = {
      history: emptyTerminalHistory as TerminalHistoryEntry[],
      listeners: new Set(),
    };
    terminalHistoryById.set(terminalId, store);
  }

  return store;
}

function reconcileStaleRunningEntries(store: TerminalHistoryStore) {
  let changed = false;
  const history = store.history.map((entry) => {
    if (entry.status !== "running" || inFlightTerminalEntries.has(entry.id)) {
      return entry;
    }

    changed = true;
    return { ...entry, status: "error" as const, error: "Interrupted" };
  });

  if (changed) {
    store.history = history;
  }

  return changed;
}

function getTerminalHistorySnapshot(terminalId: string) {
  return terminalHistoryById.get(terminalId)?.history ?? emptyTerminalHistory;
}

function subscribeTerminalHistory(terminalId: string, listener: () => void) {
  const store = getOrCreateTerminalHistoryStore(terminalId);
  const changed = reconcileStaleRunningEntries(store);
  store.listeners.add(listener);
  if (changed) {
    listener();
  }

  return () => {
    terminalHistoryById.get(terminalId)?.listeners.delete(listener);
    pruneClosedTerminalHistory(terminalId);
  };
}

function setTerminalHistory(
  terminalId: string,
  updater: (history: TerminalHistoryEntry[]) => TerminalHistoryEntry[],
) {
  const store = getOrCreateTerminalHistoryStore(terminalId);
  const nextHistory = updater(store.history);
  store.history = nextHistory;
  for (const listener of Array.from(store.listeners)) {
    listener();
  }
  // Intentionally NOT calling `pruneClosedTerminalHistory(terminalId)`
  // here. Three prune sites already cover every cleanup path without
  // coupling prune to the listener-notification loop:
  //
  // 1. `subscribeTerminalHistory`'s cleanup closure runs when a
  //    component unmounts and drops its subscription. This is the usual
  //    "panel closed while idle" case.
  // 2. The App-level `pruneTerminalPanelHistory` effect keyed on
  //    `terminalTabIdsKey` runs whenever the set of live terminal tabs
  //    changes, catching tab close even when the panel's own unmount
  //    ordering races the effect's dep-change check.
  // 3. `handleSubmit`'s `finally` block calls
  //    `pruneClosedTerminalHistory(terminalId)` once the in-flight entry
  //    count has been decremented. This is the load-bearing path for a
  //    command that finishes AFTER the panel has unmounted: the earlier
  //    subscribe-cleanup prune saw a store with a running entry and
  //    bailed out, and the completion callback needs to re-run prune so
  //    the store gets collected now that the running entry is done.
  //
  // Calling prune from inside this listener-notification loop would
  // open a re-entrant race on top of all three: a listener that
  // unsubscribes during its own notification can drop `listeners.size`
  // to zero mid-iteration, after which the prune would delete the store
  // the outer iteration is still writing into. Omitting the call here
  // eliminates the race by construction without losing any cleanup
  // coverage, because the three sites above collectively dominate it.
}

function updateTerminalHistoryEntry(
  terminalId: string,
  entryId: string,
  updater: (entry: TerminalHistoryEntry) => TerminalHistoryEntry,
) {
  setTerminalHistory(terminalId, (history) =>
    history.map((entry) => (entry.id === entryId ? updater(entry) : entry)),
  );
}

function appendTerminalOutput(
  entry: TerminalHistoryEntry,
  output: TerminalCommandOutputEvent,
) {
  if (output.stream === "stdout") {
    return {
      ...entry,
      stdout: `${entry.stdout ?? entry.response?.stdout ?? ""}${output.text}`,
    };
  }

  if (output.stream === "stderr") {
    return {
      ...entry,
      stderr: `${entry.stderr ?? entry.response?.stderr ?? ""}${output.text}`,
    };
  }

  return assertNever(output.stream, "Unhandled terminal output stream");
}

function hasRunningTerminalEntry(history: readonly TerminalHistoryEntry[]) {
  return history.some((entry) => entry.status === "running");
}

function pruneClosedTerminalHistory(terminalId: string) {
  if (!liveTerminalIds || liveTerminalIds.has(terminalId)) {
    return;
  }

  const store = terminalHistoryById.get(terminalId);
  if (store && store.listeners.size === 0 && !hasRunningTerminalEntry(store.history)) {
    terminalHistoryById.delete(terminalId);
  }
}

export function pruneTerminalPanelHistory(activeTerminalIds: Iterable<string>) {
  liveTerminalIds = new Set(activeTerminalIds);
  for (const terminalId of Array.from(terminalHistoryById.keys())) {
    pruneClosedTerminalHistory(terminalId);
  }
}

export function resetTerminalPanelStateForTests() {
  terminalHistoryById.clear();
  inFlightTerminalEntries.clear();
  liveTerminalIds = null;
}

export function setTerminalPanelHistoryForTests(
  terminalId: string,
  history: TerminalHistoryEntry[],
) {
  terminalHistoryById.set(terminalId, {
    history,
    listeners: new Set(),
  });
}

export function getTerminalPanelHistoryForTests(terminalId: string) {
  return terminalHistoryById.get(terminalId)?.history ?? null;
}

/**
 * Test-only helper that returns the number of live subscribers currently
 * attached to a terminal store. Returns `null` if no store entry exists for
 * the id. Used by the mounted-panel prune test to assert that
 * `pruneClosedTerminalHistory`'s `listeners.size === 0` gate was actually
 * exercised — without this helper, a wipe-and-recreate refactor (delete
 * the store, recreate it when the next `setTerminalHistory` runs) could
 * leave no observable trace beyond the component's own re-subscription.
 */
export function getTerminalPanelListenerCountForTests(terminalId: string) {
  return terminalHistoryById.get(terminalId)?.listeners.size ?? null;
}

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
  // `useCallback` keyed on `terminalId` so that React does not tear down
  // and re-subscribe on every render. The subscribe path runs
  // `reconcileStaleRunningEntries` each time it fires; re-subscribing on
  // every commit would double that work under StrictMode for no benefit.
  const subscribe = useCallback(
    (listener: () => void) => subscribeTerminalHistory(terminalId, listener),
    [terminalId],
  );
  const getSnapshot = useCallback(
    () => getTerminalHistorySnapshot(terminalId),
    [terminalId],
  );
  const history = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const isRunning = history.some((entry) => entry.status === "running");
  const isRunningRef = useRef(isRunning);
  // `workdirDraftDirtyRef` is the one remaining guard for the workdir draft
  // sync effect: once the user edits the input the draft is "dirty" and we
  // stop reconciling it with the incoming prop until they either submit (the
  // Use button) or the parent explicitly clears the draft. Dirty drafts
  // intentionally outlast blur, so that clicking away from the input and back
  // does not silently discard typed edits on the next prop reconcile.
  const workdirDraftDirtyRef = useRef(false);
  const shouldStickToBottomRef = useRef(true);
  const canRun = Boolean(commandDraft.trim() && normalizedWorkdir && hasScope && !isRunning);
  const terminalLabel = useMemo(
    () => formatTerminalWorkdirLabel(normalizedWorkdir),
    [normalizedWorkdir],
  );

  useEffect(() => {
    isRunningRef.current = isRunning;
  }, [isRunning]);

  useEffect(() => {
    if (workdirDraftDirtyRef.current) {
      return;
    }

    setWorkdirDraft(normalizedWorkdir);
  }, [normalizedWorkdir]);

  useLayoutEffect(() => {
    const node = scrollRef.current;
    if (!node || !shouldStickToBottomRef.current) {
      return;
    }
    scrollTerminalHistoryToBottom(node);
  }, [history]);

  const setScrollNode = useCallback((node: HTMLDivElement | null) => {
    scrollRef.current = node;
    if (node && shouldStickToBottomRef.current) {
      scrollTerminalHistoryToBottom(node);
    }
  }, []);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const command = commandDraft.trim();
    if (!command || !normalizedWorkdir || !hasScope || isRunningRef.current) {
      return;
    }

    isRunningRef.current = true;
    const entryId = crypto.randomUUID();
    const startedAt = new Date().toLocaleTimeString();
    const entry: TerminalHistoryEntry = {
      command,
      error: null,
      id: entryId,
      response: null,
      startedAt,
      status: "running",
      stderr: "",
      stdout: "",
      workdir: normalizedWorkdir,
    };
    inFlightTerminalEntries.add(entryId);
    setTerminalHistory(terminalId, (current) => [...current, entry]);
    setCommandDraft("");

    try {
      const response = await runTerminalCommandStream(
        {
          command,
          projectId: normalizedProjectId || null,
          sessionId: normalizedSessionId || null,
          workdir: normalizedWorkdir,
        },
        {
          onOutput: (output) => {
            updateTerminalHistoryEntry(terminalId, entryId, (item) =>
              appendTerminalOutput(item, output),
            );
          },
        },
      );
      updateTerminalHistoryEntry(terminalId, entryId, (item) => ({
        ...item,
        response,
        stderr: response.stderr,
        stdout: response.stdout,
        status: "done" as const,
        workdir: response.workdir,
      }));
    } catch (error) {
      updateTerminalHistoryEntry(terminalId, entryId, (item) => ({
        ...item,
        error: formatTerminalCommandError(error),
        status: "error" as const,
      }));
    } finally {
      inFlightTerminalEntries.delete(entryId);
      const store = terminalHistoryById.get(terminalId);
      isRunningRef.current = Boolean(
        store && hasRunningTerminalEntry(store.history),
      );
      pruneClosedTerminalHistory(terminalId);
    }
  }

  function clearHistory() {
    // Reuse the shared constant so repeated clears do not introduce new
    // array identities that React would have to reconcile.
    setTerminalHistory(terminalId, () => emptyTerminalHistory as TerminalHistoryEntry[]);
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
              workdirDraftDirtyRef.current = false;
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
            onChange={(event) => {
              workdirDraftDirtyRef.current = true;
              setWorkdirDraft(event.target.value);
            }}
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

      <div
        ref={setScrollNode}
        className="terminal-history"
        role="log"
        aria-live="polite"
        onScroll={(event) => {
          shouldStickToBottomRef.current = isTerminalHistoryScrolledToBottom(
            event.currentTarget,
          );
        }}
      >
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
  const stdout = entry.stdout ?? response?.stdout ?? "";
  const stderr = entry.stderr ?? response?.stderr ?? "";
  const statusLabel =
    entry.status === "running"
      ? "Running"
      : entry.status === "error"
        ? "Failed"
        : formatTerminalResult(response);

  return (
    <article className={`terminal-history-item is-${entry.status}`}>
      <div className="terminal-io-row terminal-io-row-input">
        <span className="terminal-io-label" aria-hidden="true">
          IN
        </span>
        <div className="terminal-io-body">
          <div className="terminal-io-heading">
            <code className="terminal-command-text">{entry.command}</code>
            <span className="terminal-history-meta">
              {entry.startedAt} - {statusLabel}
            </span>
          </div>
          <div className="terminal-history-workdir" title={entry.workdir}>
            {entry.workdir}
          </div>
        </div>
      </div>
      <div className="terminal-io-row terminal-io-row-output">
        <span className="terminal-io-label" aria-hidden="true">
          OUT
        </span>
        <div className="terminal-io-body">
          {entry.error ? (
            <pre className="terminal-output terminal-output-error">{entry.error}</pre>
          ) : null}
          {stdout ? (
            <pre className="terminal-output terminal-output-stdout">{stdout}</pre>
          ) : null}
          {stderr ? (
            <pre className="terminal-output terminal-output-stderr">{stderr}</pre>
          ) : null}
          {response?.outputTruncated ? (
            <div className="terminal-output-note">Output was truncated.</div>
          ) : null}
          {entry.status === "running" && !stdout && !stderr ? (
            <div className="terminal-output-note">Waiting for command output...</div>
          ) : null}
        </div>
      </div>
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

function formatTerminalCommandError(error: unknown) {
  const message = getErrorMessage(error);
  if (error instanceof ApiRequestError && error.status === 429) {
    return `${message} (rate limit - try again in a moment)`;
  }

  return message;
}

function formatDuration(durationMs: number) {
  if (durationMs < 1000) {
    return `${durationMs}ms`;
  }

  return `${(durationMs / 1000).toFixed(1)}s`;
}

function isTerminalHistoryScrolledToBottom(node: HTMLElement) {
  const bottomGap = node.scrollHeight - node.clientHeight - node.scrollTop;
  return bottomGap <= 4;
}

function scrollTerminalHistoryToBottom(node: HTMLElement) {
  const nextScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
  if (Math.abs(node.scrollTop - nextScrollTop) > 1) {
    node.scrollTop = nextScrollTop;
  }
}

export function formatTerminalWorkdirLabel(workdir: string) {
  if (!workdir) {
    return "No working directory";
  }

  const segments = workdir.split(/[/\\]+/).filter(Boolean);
  return segments[segments.length - 1] ?? workdir;
}
