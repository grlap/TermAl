// Owns the session-list adapter to the existing measured row window.
// Filtering, grouping, actions and navigation remain in AppControlSurface.
import { useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useWorkWindow } from "./panels/use-work-window";
import { formatSessionOrchestratorGroupName, type ControlSurfaceSessionListEntry } from "./control-surface-state";
import type { Session } from "./types";
import "./session-list-window.css";

type Group = Extract<ControlSurfaceSessionListEntry, { kind: "orchestratorGroup" }>;
type Row = { key: string; group: Group | null } & (
  | { kind: "session"; session: Session; last: boolean }
  | { kind: "header" | "footer" }
);
const sessionKey = (id: string) => `session:${id}`;
const focusable = (node: HTMLElement) => Array.from(node.querySelectorAll<HTMLElement>(
  'button:not(:disabled),a[href],input:not(:disabled),select:not(:disabled),textarea:not(:disabled),[tabindex="0"]',
)).filter(element => getComputedStyle(element).display !== "none" && getComputedStyle(element).visibility !== "hidden");
const focusTarget = (node: HTMLElement, backward: boolean) => {
  const elements = focusable(node);
  return elements[backward ? elements.length - 1 : 0];
};

export function SessionListWindow({ surfaceId, entries, collapsedIds, selectedSessionId, pendingSessionId, actionSessionId, renderSession, renderGroupHeader, children }: {
  surfaceId: string;
  entries: readonly ControlSurfaceSessionListEntry[];
  collapsedIds: readonly string[];
  selectedSessionId?: string | null;
  pendingSessionId?: string | null;
  actionSessionId?: string | null;
  renderSession: (session: Session) => ReactNode;
  renderGroupHeader: (group: Group) => ReactNode;
  children?: ReactNode;
}) {
  const viewport = useRef<HTMLDivElement>(null);
  const [interactionKey, setInteractionKey] = useState<string>();
  const [pendingFocus, setPendingFocus] = useState<{ key: string; backward: boolean }>();
  const rows = useMemo(() => {
    const result: Row[] = [];
    for (const entry of entries) {
      if (entry.kind === "session") result.push({ key: sessionKey(entry.session.id), kind: "session", session: entry.session, group: null, last: false });
      else {
        result.push({ key: `group:${entry.orchestrator.id}`, kind: "header", group: entry });
        if (!collapsedIds.includes(entry.orchestrator.id)) entry.sessions.forEach((session, index) => {
          result.push({ key: sessionKey(session.id), kind: "session", session, group: entry, last: index === entry.sessions.length - 1 });
        });
        result.push({ key: `end:${entry.orchestrator.id}`, kind: "footer", group: entry });
      }
    }
    return result;
  }, [entries, collapsedIds]);
  const keys = useMemo(() => rows.map(row => row.key), [rows]);
  // Native Tab enters through the logical endpoints, even when scrolled away.
  // Retain at most two endpoint rows alongside action/drag and reveal targets.
  const retained = useMemo(() => [rows[0]?.key,
    rows[rows.length - (rows[rows.length - 1]?.kind === "footer" ? 2 : 1)]?.key,
    interactionKey, pendingFocus?.key, pendingSessionId && sessionKey(pendingSessionId), actionSessionId && sessionKey(actionSessionId)]
    .filter((key): key is string => !!key), [rows, interactionKey, pendingFocus, pendingSessionId, actionSessionId]);
  const rowWindow = useWorkWindow(keys, viewport, selectedSessionId ? sessionKey(selectedSessionId) : undefined, retained);
  useLayoutEffect(() => {
    if (!pendingFocus) return;
    // One committed reveal attempt only: removed/filtered targets cannot steal
    // focus later. The retained key makes a surviving target mount this render.
    setPendingFocus(undefined);
    const node = Array.from(viewport.current?.querySelectorAll<HTMLElement>("[data-work-window-key]") ?? [])
      .find(element => element.dataset.workWindowKey === pendingFocus.key);
    const target = node && focusTarget(node, pendingFocus.backward);
    target?.focus({ preventScroll: true });
  }, [pendingFocus]);
  const rememberTrigger = (target: EventTarget | null) => {
    setPendingFocus(undefined);
    if (target instanceof Element) setInteractionKey(target.closest<HTMLElement>("[data-work-window-key]")?.dataset.workWindowKey);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    setPendingFocus(undefined);
    if (!rowWindow.enabled || event.key !== "Tab" || event.altKey || event.ctrlKey || event.metaKey || !(event.target instanceof HTMLElement)) return;
    const node = event.target.closest<HTMLElement>("[data-work-window-key]");
    if (!node) return;
    const buttons = focusable(node);
    const edge = event.shiftKey ? buttons[0] : buttons[buttons.length - 1];
    if (edge !== document.activeElement) return;
    const direction = event.shiftKey ? -1 : 1;
    let index = keys.indexOf(node.dataset.workWindowKey!) + direction;
    while (rows[index]?.kind === "footer") index += direction;
    const next = rows[index];
    if (!next) return; // Native Tab leaves the list at its logical boundaries.
    event.preventDefault();
    const mounted = Array.from(viewport.current?.querySelectorAll<HTMLElement>("[data-work-window-key]") ?? [])
      .find(element => element.dataset.workWindowKey === next.key);
    const mountedTarget = mounted && focusTarget(mounted, event.shiftKey);
    if (mountedTarget) { mountedTarget.focus(); return; }
    setPendingFocus({ key: next.key, backward: event.shiftKey });
    rowWindow.reveal(next.key);
  };
  const spacer = (height: number, key: string) => height > 0 ? <div key={key} aria-hidden="true" style={{ height }} /> : null;
  let content: ReactNode;
  if (rows.length === 0) content = children;
  else {
    const result: ReactNode[] = [];
    let current: Group | null = null;
    let grouped: ReactNode[] = [];
    let header: ReactNode = null;
    const flush = () => {
      if (current) result.push(<section key={`group:${current.orchestrator.id}`} className="session-orchestrator-group session-window-group" role="group"
        aria-label={`Orchestration ${formatSessionOrchestratorGroupName(current.orchestrator)}`} data-status={current.orchestrator.status}>{header}<div className="session-orchestrator-group-list" id={`${surfaceId}-orchestrator-group-list-${current.orchestrator.id}`}>{grouped}</div></section>);
      grouped = []; current = null; header = null;
    };
    rowWindow.indices.forEach((index, position) => {
      const row = rows[index];
      const gap = spacer(rowWindow.before(position), `gap:${row.key}`);
      if (row.group !== current) { flush(); result.push(gap); current = row.group; }
      else if (current) grouped.push(gap);
      else result.push(gap);
      const item = <div key={row.key} ref={rowWindow.rowRef(row.key)}
        className={row.kind === "header" ? `session-window-header${row.group && collapsedIds.includes(row.group.orchestrator.id) ? " collapsed" : ""}` : row.kind === "footer" ? "session-window-footer" : row.group ? `session-window-group-row${row.kind === "session" && row.last ? " last" : ""}` : "session-window-row"}>
        {row.kind === "session" ? renderSession(row.session) : row.kind === "header" && row.group ? renderGroupHeader(row.group) : null}
      </div>;
      // The inter-card gap is outside the group's painted background.
      if (row.kind === "footer") { flush(); result.push(item); }
      else if (current && row.kind === "header") header = item;
      else if (current) grouped.push(item); else result.push(item);
    });
    flush(); result.push(spacer(rowWindow.after, "after")); content = result;
  }
  return <div ref={viewport} className="session-list session-list-windowed" data-windowed={rowWindow.enabled} onKeyDownCapture={onKeyDown}
    onBlurCapture={event => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setPendingFocus(undefined); }}
    onPointerDownCapture={event => rememberTrigger(event.target)} onContextMenuCapture={event => rememberTrigger(event.target)} onDragStartCapture={event => rememberTrigger(event.target)}>
    <div className="session-list-entries" ref={rowWindow.content}>{content}</div>
  </div>;
}
