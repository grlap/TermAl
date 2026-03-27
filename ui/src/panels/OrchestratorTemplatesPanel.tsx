import { useEffect, useLayoutEffect, useMemo, useRef, useState, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";

import { AgentIcon } from "../agent-icon";
import {
  createOrchestratorTemplate,
  deleteOrchestratorTemplate,
  fetchOrchestratorTemplates,
  updateOrchestratorTemplate,
} from "../api";
import { sanitizeUserFacingErrorMessage } from "../error-messages";
import { dispatchOrchestratorTemplatesChangedEvent } from "../orchestrator-templates-events";
import type {
  AgentType,
  OrchestratorNodePosition,
  OrchestratorSessionTemplate,
  OrchestratorTemplate,
  OrchestratorTemplateDraft,
  OrchestratorTemplateTransition,
  OrchestratorTransitionResultMode,
} from "../types";

const BOARD_WIDTH = 2560;
const BOARD_HEIGHT = 1600;
const CARD_WIDTH = 320;
const CARD_HEIGHT = 196;
const BOARD_MARGIN = 32;
const MIN_ZOOM = 0.5;
const MAX_ZOOM = 2;
const DEFAULT_ZOOM = 1;
const WHEEL_ZOOM_SENSITIVITY = 0.002;
const PAN_CONTEXT_MENU_SUPPRESS_THRESHOLD_PX = 4;
const STATE_KEY_PREFIX = "termal-orchestrator-panel-state:";

function clampZoom(value: number): number {
  return Math.round(Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, value)) * 1000) / 1000;
}

type ZoomAnchor = {
  canvasX: number;
  canvasY: number;
  clientOffsetX: number;
  clientOffsetY: number;
  scrollContainer: HTMLElement;
  scrollLeft: number;
  scrollTop: number;
};

type PanDragState = {
  hasMoved: boolean;
  originScrollLeft: number;
  originScrollTop: number;
  pointerId: number;
  scrollContainer: HTMLElement;
  startClientX: number;
  startClientY: number;
};
const AGENT_OPTIONS: ReadonlyArray<{ label: string; value: AgentType }> = [
  { label: "Claude", value: "Claude" },
  { label: "Codex", value: "Codex" },
  { label: "Cursor", value: "Cursor" },
  { label: "Gemini", value: "Gemini" },
];
const RESULT_MODE_OPTIONS: ReadonlyArray<{
  label: string;
  value: OrchestratorTransitionResultMode;
}> = [
  { label: "Last response", value: "lastResponse" },
  { label: "Summary", value: "summary" },
  { label: "Summary + last response", value: "summaryAndLastResponse" },
  { label: "No result", value: "none" },
];

type DragState = {
  nodeId: string;
  pointerId: number;
  originX: number;
  originY: number;
  deltaX: number;
  deltaY: number;
  startClientX: number;
  startClientY: number;
};

type ConnectionDragState = {
  fromSessionId: string;
  anchorSide: AnchorSide;
  pointerId: number;
  cursorX: number;
  cursorY: number;
};

type AnchorSide = "top" | "right" | "bottom" | "left";

const ANCHOR_SIDES: readonly AnchorSide[] = ["top", "right", "bottom", "left"];

const CONNECTOR_DOT_OFFSET = 18;

type TransitionGeometry = {
  transition: OrchestratorTemplateTransition;
  path: string;
  midpointX: number;
  midpointY: number;
  noteX: number;
  noteY: number;
  title: string;
};

type PanelState = {
  draft: OrchestratorTemplateDraft;
  selectedNodeId: string | null;
  selectedTemplateId: string | null;
};

export function OrchestratorTemplatesPanel({
  initialTemplateId = null,
  persistenceKey = null,
  startMode = "browse",
}: {
  initialTemplateId?: string | null;
  persistenceKey?: string | null;
  startMode?: "browse" | "edit" | "new";
}) {
  const [templates, setTemplates] = useState<OrchestratorTemplate[]>([]);
  const [selectedTemplateId, setSelectedTemplateId] = useState<string | null>(null);
  const [draft, setDraft] = useState<OrchestratorTemplateDraft>(emptyDraft);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [dragState, setDragState] = useState<DragState | null>(null);
  const [isPanning, setIsPanning] = useState(false);
  const [connectionDrag, setConnectionDrag] = useState<ConnectionDragState | null>(null);
  const [zoom, setZoom] = useState(DEFAULT_ZOOM);
  const panDragStateRef = useRef<PanDragState | null>(null);
  const suppressPanContextMenuRef = useRef(false);
  const zoomAnchorRef = useRef<ZoomAnchor | null>(null);
  const scaleFrameRef = useRef<HTMLDivElement | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);

  const selectedTemplate = useMemo(
    () => templates.find((template) => template.id === selectedTemplateId) ?? null,
    [selectedTemplateId, templates],
  );
  const renderedSessions = useMemo(
    () =>
      draft.sessions.map((session) => {
        if (!dragState || dragState.nodeId !== session.id) {
          return session;
        }
        return {
          ...session,
          position: {
            x: session.position.x + dragState.deltaX,
            y: session.position.y + dragState.deltaY,
          },
        };
      }),
    [draft.sessions, dragState],
  );
  const sessionLookup = useMemo(
    () => new Map(renderedSessions.map((session) => [session.id, session])),
    [renderedSessions],
  );
  const transitionGeometries = useMemo(
    () =>
      draft.transitions.flatMap((transition) => {
        const fromNode = sessionLookup.get(transition.fromSessionId);
        const toNode = sessionLookup.get(transition.toSessionId);
        if (!fromNode || !toNode) {
          return [];
        }
        return [buildTransitionGeometry(transition, fromNode, toNode)];
      }),
    [draft.transitions, sessionLookup],
  );
  const stateKey = persistenceKey?.trim() ? `${STATE_KEY_PREFIX}${persistenceKey.trim()}` : null;
  const validationError = validateDraft(draft);
  const referenceDraft = selectedTemplate ? templateToDraft(selectedTemplate) : emptyDraft();
  const isDirty = JSON.stringify(draft) !== JSON.stringify(referenceDraft);
  const isCanvasMode = startMode !== "browse";

  useEffect(() => {
    let cancelled = false;
    async function load() {
      setIsLoading(true);
      setErrorMessage(null);
      try {
        const response = await fetchOrchestratorTemplates();
        if (cancelled) {
          return;
        }
        setTemplates(response.templates);
        const restored = stateKey ? readState(stateKey) : null;
        const initial = resolveInitialState(response.templates, initialTemplateId, restored, startMode);
        setDraft(initial.draft);
        setSelectedNodeId(initial.selectedNodeId);
        setSelectedTemplateId(initial.selectedTemplateId);
      } catch (error) {
        if (!cancelled) {
          setErrorMessage(getErrorMessage(error));
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    }
    void load();
    return () => {
      cancelled = true;
    };
  }, [initialTemplateId, startMode, stateKey]);

  useEffect(() => {
    if (!stateKey || isLoading) {
      return;
    }
    window.localStorage.setItem(
      stateKey,
      JSON.stringify({ draft, selectedNodeId, selectedTemplateId } satisfies PanelState),
    );
  }, [draft, isLoading, selectedNodeId, selectedTemplateId, stateKey]);

  useEffect(() => {
    const frame = scaleFrameRef.current;
    if (!frame) {
      return;
    }

    function handleBoardWheel(event: WheelEvent) {
      if (event.defaultPrevented || !event.ctrlKey) {
        return;
      }

      const frameNode = event.currentTarget;
      if (!(frameNode instanceof HTMLElement)) {
        return;
      }

      const scrollContainer = frameNode.closest(".orchestrator-board-scroll");
      if (!(scrollContainer instanceof HTMLElement)) {
        return;
      }

      event.preventDefault();
      const nextZoom = clampZoom(zoom * Math.exp(-event.deltaY * WHEEL_ZOOM_SENSITIVITY));
      if (nextZoom === zoom) {
        return;
      }

      const rect = frameNode.getBoundingClientRect();
      const clientOffsetX = event.clientX - rect.left;
      const clientOffsetY = event.clientY - rect.top;
      zoomAnchorRef.current = {
        canvasX: clientOffsetX / zoom,
        canvasY: clientOffsetY / zoom,
        clientOffsetX,
        clientOffsetY,
        scrollContainer,
        scrollLeft: scrollContainer.scrollLeft,
        scrollTop: scrollContainer.scrollTop,
      };
      setZoom(nextZoom);
    }

    frame.addEventListener("wheel", handleBoardWheel, { passive: false });
    return () => {
      frame.removeEventListener("wheel", handleBoardWheel);
    };
  }, [zoom]);

  useLayoutEffect(() => {
    const anchor = zoomAnchorRef.current;
    if (!anchor) {
      return;
    }

    zoomAnchorRef.current = null;
    anchor.scrollContainer.scrollLeft = Math.max(
      anchor.scrollLeft + anchor.canvasX * zoom - anchor.clientOffsetX,
      0,
    );
    anchor.scrollContainer.scrollTop = Math.max(
      anchor.scrollTop + anchor.canvasY * zoom - anchor.clientOffsetY,
      0,
    );
  }, [zoom]);

  async function saveTemplate() {
    if (validationError) {
      setErrorMessage(validationError);
      return;
    }
    setIsSaving(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      if (selectedTemplateId) {
        const response = await updateOrchestratorTemplate(selectedTemplateId, draft);
        setTemplates((current) => current.map((template) => (
          template.id === response.template.id ? response.template : template
        )));
        setDraft(templateToDraft(response.template));
        setSelectedNodeId(response.template.sessions[0]?.id ?? null);
        setStatusMessage("Template saved.");
      } else {
        const response = await createOrchestratorTemplate(draft);
        setTemplates((current) => [response.template, ...current]);
        setSelectedTemplateId(response.template.id);
        setDraft(templateToDraft(response.template));
        setSelectedNodeId(response.template.sessions[0]?.id ?? null);
        setStatusMessage("Template created.");
      }
      dispatchOrchestratorTemplatesChangedEvent();
    } catch (error) {
      setErrorMessage(getErrorMessage(error));
    } finally {
      setIsSaving(false);
    }
  }

  async function removeTemplate() {
    if (!selectedTemplateId) {
      return;
    }
    setIsDeleting(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      const response = await deleteOrchestratorTemplate(selectedTemplateId);
      setTemplates(response.templates);
      const nextTemplate = response.templates[0] ?? null;
      if (nextTemplate) {
        setSelectedTemplateId(nextTemplate.id);
        setDraft(templateToDraft(nextTemplate));
        setSelectedNodeId(nextTemplate.sessions[0]?.id ?? null);
      } else {
        setSelectedTemplateId(null);
        setDraft(emptyDraft());
        setSelectedNodeId(null);
      }
      setStatusMessage("Template deleted.");
      dispatchOrchestratorTemplatesChangedEvent();
    } catch (error) {
      setErrorMessage(getErrorMessage(error));
    } finally {
      setIsDeleting(false);
    }
  }

  function setSessionField<K extends keyof OrchestratorSessionTemplate>(
    sessionId: string,
    key: K,
    value: OrchestratorSessionTemplate[K],
  ) {
    setDraft((current) => ({
      ...current,
      sessions: current.sessions.map((session) => session.id === sessionId ? { ...session, [key]: value } : session),
    }));
  }

  function setSessionId(sessionId: string, nextId: string) {
    setDraft((current) => ({
      ...current,
      sessions: current.sessions.map((session) => session.id === sessionId ? { ...session, id: nextId } : session),
      transitions: current.transitions.map((transition) => ({
        ...transition,
        fromSessionId: transition.fromSessionId === sessionId ? nextId : transition.fromSessionId,
        toSessionId: transition.toSessionId === sessionId ? nextId : transition.toSessionId,
      })),
    }));
    setSelectedNodeId((current) => current === sessionId ? nextId : current);
  }

  function addSession() {
    setDraft((current) => {
      const nextSession = createSession(current.sessions);
      setSelectedNodeId(nextSession.id);
      return { ...current, sessions: [...current.sessions, nextSession] };
    });
  }

  function removeSession(sessionId: string) {
    setDraft((current) => ({
      ...current,
      sessions: current.sessions.filter((session) => session.id !== sessionId),
      transitions: current.transitions.filter((transition) => transition.fromSessionId !== sessionId && transition.toSessionId !== sessionId),
    }));
    setSelectedNodeId((current) => current === sessionId ? null : current);
  }

  function setTransitionField<K extends keyof OrchestratorTemplateTransition>(
    transitionId: string,
    key: K,
    value: OrchestratorTemplateTransition[K],
  ) {
    setDraft((current) => ({
      ...current,
      transitions: current.transitions.map((transition) => transition.id === transitionId ? { ...transition, [key]: value } : transition),
    }));
  }

  function addTransition() {
    setDraft((current) => ({
      ...current,
      transitions: [...current.transitions, createTransition(current.sessions, current.transitions)],
    }));
  }

  function startDrag(session: OrchestratorSessionTemplate, event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0) {
      return;
    }
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    setSelectedNodeId(session.id);
    setDragState({
      nodeId: session.id,
      pointerId: event.pointerId,
      originX: session.position.x,
      originY: session.position.y,
      deltaX: 0,
      deltaY: 0,
      startClientX: event.clientX,
      startClientY: event.clientY,
    });
  }

  function updateDrag(sessionId: string, event: ReactPointerEvent<HTMLDivElement>) {
    setDragState((current) => {
      if (!current || current.nodeId !== sessionId || current.pointerId !== event.pointerId) {
        return current;
      }
      return { ...current, deltaX: event.clientX - current.startClientX, deltaY: event.clientY - current.startClientY };
    });
  }

  function finishDrag(sessionId: string, event: ReactPointerEvent<HTMLDivElement>, cancelled = false) {
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    setDragState((current) => {
      if (!current || current.nodeId !== sessionId || current.pointerId !== event.pointerId) {
        return current;
      }
      if (!cancelled) {
        setSessionField(sessionId, "position", clampPosition(current.originX + current.deltaX, current.originY + current.deltaY));
      }
      return null;
    });
  }

  function startConnectionDrag(sessionId: string, side: AnchorSide, event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);

    const surfaceNode = event.currentTarget.closest(".orchestrator-board-surface");
    if (!surfaceNode) {
      return;
    }
    const surfaceRect = surfaceNode.getBoundingClientRect();
    const canvasX = (event.clientX - surfaceRect.left) / zoom;
    const canvasY = (event.clientY - surfaceRect.top) / zoom;

    setConnectionDrag({
      fromSessionId: sessionId,
      anchorSide: side,
      pointerId: event.pointerId,
      cursorX: canvasX,
      cursorY: canvasY,
    });
  }

  function updateConnectionDrag(event: ReactPointerEvent<HTMLDivElement>) {
    setConnectionDrag((current) => {
      if (!current || current.pointerId !== event.pointerId) {
        return current;
      }
      const surfaceNode = event.currentTarget.closest(".orchestrator-board-surface");
      if (!surfaceNode) {
        return current;
      }
      const surfaceRect = surfaceNode.getBoundingClientRect();
      return {
        ...current,
        cursorX: (event.clientX - surfaceRect.left) / zoom,
        cursorY: (event.clientY - surfaceRect.top) / zoom,
      };
    });
  }

  function finishConnectionDrag(event: ReactPointerEvent<HTMLDivElement>) {
    if (!connectionDrag || connectionDrag.pointerId !== event.pointerId) {
      return;
    }
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }

    const surfaceNode = event.currentTarget.closest(".orchestrator-board-surface");
    if (surfaceNode) {
      const surfaceRect = surfaceNode.getBoundingClientRect();
      const canvasX = (event.clientX - surfaceRect.left) / zoom;
      const canvasY = (event.clientY - surfaceRect.top) / zoom;

      const targetSession = renderedSessions.find(
        (session) =>
          session.id !== connectionDrag.fromSessionId &&
          canvasX >= session.position.x &&
          canvasX <= session.position.x + CARD_WIDTH &&
          canvasY >= session.position.y &&
          canvasY <= session.position.y + CARD_HEIGHT,
      );

      if (targetSession) {
        const alreadyExists = draft.transitions.some(
          (transition) =>
            transition.fromSessionId === connectionDrag.fromSessionId &&
            transition.toSessionId === targetSession.id,
        );
        if (!alreadyExists) {
          setDraft((current) => ({
            ...current,
            transitions: [
              ...current.transitions,
              createTransitionBetween(connectionDrag.fromSessionId, targetSession.id, current.transitions),
            ],
          }));
        }
      }
    }

    setConnectionDrag(null);
  }

  function startCanvasPan(event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 2) {
      return;
    }

    const scrollContainer = event.currentTarget.closest(".orchestrator-board-scroll");
    if (!(scrollContainer instanceof HTMLElement)) {
      return;
    }

    event.preventDefault();
    suppressPanContextMenuRef.current = false;
    event.currentTarget.setPointerCapture(event.pointerId);
    panDragStateRef.current = {
      hasMoved: false,
      originScrollLeft: scrollContainer.scrollLeft,
      originScrollTop: scrollContainer.scrollTop,
      pointerId: event.pointerId,
      scrollContainer,
      startClientX: event.clientX,
      startClientY: event.clientY,
    };
    setIsPanning(true);
  }

  function updateCanvasPan(event: ReactPointerEvent<HTMLDivElement>) {
    const current = panDragStateRef.current;
    if (!current || current.pointerId !== event.pointerId) {
      return;
    }

    event.preventDefault();
    const deltaX = event.clientX - current.startClientX;
    const deltaY = event.clientY - current.startClientY;
    current.scrollContainer.scrollLeft = Math.max(current.originScrollLeft - deltaX, 0);
    current.scrollContainer.scrollTop = Math.max(current.originScrollTop - deltaY, 0);
    if (
      !current.hasMoved &&
      (Math.abs(deltaX) >= PAN_CONTEXT_MENU_SUPPRESS_THRESHOLD_PX ||
        Math.abs(deltaY) >= PAN_CONTEXT_MENU_SUPPRESS_THRESHOLD_PX)
    ) {
      current.hasMoved = true;
      suppressPanContextMenuRef.current = true;
    }
  }

  function finishCanvasPan(event: ReactPointerEvent<HTMLDivElement>, cancelled = false) {
    const current = panDragStateRef.current;
    if (!current || current.pointerId !== event.pointerId) {
      return;
    }

    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }

    if (!cancelled && current.hasMoved) {
      suppressPanContextMenuRef.current = true;
    }

    panDragStateRef.current = null;
    setIsPanning(false);
  }

  function handleCanvasContextMenu(event: ReactMouseEvent<HTMLDivElement>) {
    if (!isPanning && !suppressPanContextMenuRef.current) {
      return;
    }

    event.preventDefault();
    suppressPanContextMenuRef.current = false;
  }

  return (
    <section className={`settings-panel-stack orchestrator-templates-panel${isCanvasMode ? " canvas-mode" : ""}`}>
      {isCanvasMode ? null : (
        <div className="settings-panel-intro">
          <div>
            <p className="session-control-label">Reusable session graphs</p>
            <p className="settings-panel-copy">
              Design orchestration templates as connected sessions. Transitions fire when a source session replies and becomes prompt-ready again.
            </p>
          </div>
        </div>
      )}
      <div className={`orchestrator-template-shell${isCanvasMode ? " canvas-mode" : ""}`}>
        {isCanvasMode ? null : (
          <aside className="orchestrator-template-sidebar message-card prompt-settings-card">
            <div className="orchestrator-template-sidebar-header">
              <div><div className="card-label">Library</div><h3>Templates</h3></div>
              <button className="ghost-button" type="button" onClick={() => {
                setSelectedTemplateId(null);
                setDraft(emptyDraft());
                setSelectedNodeId(null);
                setStatusMessage(null);
                setErrorMessage(null);
              }}>New template</button>
            </div>
            <div className="orchestrator-template-sidebar-list" role="list">
              {isLoading ? <p className="session-control-hint">Loading templates...</p> : templates.length === 0 ? (
                <p className="session-control-hint">No orchestration templates yet. Start a new one and save it here.</p>
              ) : templates.map((template) => (
                <button
                  key={template.id}
                  className={`orchestrator-template-list-item${template.id === selectedTemplateId ? " selected" : ""}`}
                  type="button"
                  onClick={() => {
                    setSelectedTemplateId(template.id);
                    setDraft(templateToDraft(template));
                    setSelectedNodeId(template.sessions[0]?.id ?? null);
                    setStatusMessage(null);
                    setErrorMessage(null);
                  }}
                >
                  <span className="orchestrator-template-list-item-copy">
                    <strong>{template.name}</strong>
                    <span>{template.sessions.length} sessions · {template.transitions.length} transitions</span>
                  </span>
                  <span className="orchestrator-template-list-item-meta">{template.updatedAt}</span>
                </button>
              ))}
            </div>
          </aside>
        )}
        <div className="orchestrator-template-editor">
          <article className="message-card prompt-settings-card orchestrator-template-meta-card">
            <div className="orchestrator-template-meta-header">
              <div><div className="card-label">Template</div><h3>{selectedTemplateId ? "Edit template" : "New template"}</h3></div>
              <div className="orchestrator-template-actions">
                <button className="ghost-button" type="button" onClick={() => {
                  setDraft(referenceDraft);
                  setSelectedNodeId(referenceDraft.sessions[0]?.id ?? null);
                  setStatusMessage(null);
                  setErrorMessage(null);
                }} disabled={!isDirty || isSaving}>Reset draft</button>
                <button className="ghost-button" type="button" onClick={() => void removeTemplate()} disabled={!selectedTemplateId || isDeleting || isSaving}>Delete</button>
                <button className="send-button" type="button" onClick={() => void saveTemplate()} disabled={isSaving || isDeleting || !!validationError || !isDirty}>
                  {selectedTemplateId ? (isSaving ? "Saving..." : "Save template") : isSaving ? "Creating..." : "Create template"}
                </button>
              </div>
            </div>
            <div className="orchestrator-template-meta-grid">
              <div className="session-control-group">
                <label className="session-control-label" htmlFor="orchestrator-template-name">Template name</label>
                <input id="orchestrator-template-name" className="themed-input" type="text" value={draft.name} onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))} placeholder="Feature delivery" />
              </div>
              <div className="session-control-group orchestrator-template-description-group">
                <label className="session-control-label" htmlFor="orchestrator-template-description">Description</label>
                <textarea id="orchestrator-template-description" className="themed-input orchestrator-template-textarea" rows={3} value={draft.description} onChange={(event) => setDraft((current) => ({ ...current, description: event.target.value }))} placeholder="What does this orchestration coordinate?" />
              </div>
            </div>
            <p className={`session-control-hint${errorMessage ? " error" : ""}`.trim()}>
              {errorMessage ?? validationError ?? statusMessage ?? "Add sessions, connect them with transitions, and use {{result}} inside prompt templates."}
            </p>
          </article>
          <div className="orchestrator-template-editor-grid">
            <section className="message-card prompt-settings-card orchestrator-board-panel">
              <div className="orchestrator-board-header">
                <div><div className="card-label">Layout</div><h3>Canvas</h3></div>
                <p className="session-control-hint">Drag cards to lay out the graph.</p>
              </div>
              <div className="orchestrator-board-scroll">
                <div
                  ref={scaleFrameRef}
                  className={`orchestrator-board-scale-frame${isPanning ? " panning" : ""}`}
                  style={{ width: `${BOARD_WIDTH * zoom}px`, height: `${BOARD_HEIGHT * zoom}px` }}
                  onPointerDown={startCanvasPan}
                  onPointerMove={updateCanvasPan}
                  onPointerUp={(event) => finishCanvasPan(event)}
                  onPointerCancel={(event) => finishCanvasPan(event, true)}
                  onContextMenu={handleCanvasContextMenu}
                >
                <div
                  className="orchestrator-board-surface"
                  style={{ width: `${BOARD_WIDTH}px`, height: `${BOARD_HEIGHT}px`, transform: `scale(${zoom})`, transformOrigin: "top left" }}
                >
                  <svg className="orchestrator-board-edges" width={BOARD_WIDTH} height={BOARD_HEIGHT} viewBox={`0 0 ${BOARD_WIDTH} ${BOARD_HEIGHT}`} aria-hidden="true">
                    {transitionGeometries.map((geometry) => (
                      <g key={geometry.transition.id}>
                        <path className="orchestrator-board-edge" d={geometry.path} />
                        <circle className="orchestrator-board-edge-anchor" cx={geometry.midpointX} cy={geometry.midpointY} r="5" />
                      </g>
                    ))}
                    {connectionDrag ? (() => {
                      const fromSession = renderedSessions.find((s) => s.id === connectionDrag.fromSessionId);
                      if (!fromSession) {
                        return null;
                      }
                      const anchor = anchorPosition(fromSession, connectionDrag.anchorSide);
                      return (
                        <path
                          className="orchestrator-board-edge orchestrator-board-edge-draft"
                          d={`M ${anchor.x} ${anchor.y} L ${connectionDrag.cursorX} ${connectionDrag.cursorY}`}
                        />
                      );
                    })() : null}
                  </svg>
                  <div className="orchestrator-board-transition-layer">
                    {transitionGeometries.map((geometry) => (
                      <div
                        key={`${geometry.transition.id}-note`}
                        className="orchestrator-board-transition-note"
                        style={{ left: `${geometry.noteX}px`, top: `${geometry.noteY}px` }}
                        title={geometry.title}
                      >
                        <TransitionNoteIcon />
                      </div>
                    ))}
                  </div>
                  {renderedSessions.map((session) => {
                    const isDragging = dragState?.nodeId === session.id;
                    return (
                      <article
                        key={session.id}
                        className={`orchestrator-board-card${selectedNodeId === session.id ? " selected" : ""}${isDragging ? " dragging" : ""}`}
                        style={{ left: `${session.position.x}px`, top: `${session.position.y}px`, width: `${CARD_WIDTH}px`, minHeight: `${CARD_HEIGHT}px` }}
                      >
                        <div
                          className="orchestrator-board-card-grab"
                          role="button"
                          tabIndex={0}
                          onClick={() => setSelectedNodeId(session.id)}
                          onKeyDown={(event) => {
                            if (event.key === "Enter" || event.key === " ") {
                              event.preventDefault();
                              setSelectedNodeId(session.id);
                            }
                          }}
                          onPointerDown={(event) => startDrag(session, event)}
                          onPointerMove={(event) => updateDrag(session.id, event)}
                          onPointerUp={(event) => finishDrag(session.id, event)}
                          onPointerCancel={(event) => finishDrag(session.id, event, true)}
                        >
                          <div className="orchestrator-board-card-topline">
                            <span className="orchestrator-board-card-kind"><AgentIcon agent={session.agent} className="orchestrator-board-card-agent" /><span>Session</span></span>
                            <span className="orchestrator-board-card-chip">{session.autoApprove ? "Auto" : "Review"}</span>
                          </div>
                          <h4>{session.name || session.id}</h4>
                          <p className="orchestrator-board-card-snippet">
                            {session.instructions.trim() || "No instructions yet."}
                          </p>
                        </div>
                        <div className="orchestrator-board-card-body">
                          <div className="orchestrator-board-card-meta-block">
                            <span>Session id</span>
                            <strong>{session.id}</strong>
                          </div>
                          <div className="orchestrator-board-card-meta-block">
                            <span>Model</span>
                            <strong>{session.model?.trim() ? session.model : "Agent default"}</strong>
                          </div>
                        </div>
                        {selectedNodeId === session.id && !isDragging ? (
                          ANCHOR_SIDES.map((side) => (
                            <div
                              key={side}
                              className={`orchestrator-board-connector orchestrator-board-connector-${side}`}
                              onPointerDown={(event) => startConnectionDrag(session.id, side, event)}
                              onPointerMove={updateConnectionDrag}
                              onPointerUp={finishConnectionDrag}
                              onPointerCancel={() => setConnectionDrag(null)}
                            >
                              <div className="orchestrator-board-connector-dot" />
                              <div className="orchestrator-board-connector-arrow" />
                            </div>
                          ))
                        ) : null}
                      </article>
                    );
                  })}
                </div>
                </div>
              </div>
            </section>
            <div className="orchestrator-template-inspectors">
              <article className="message-card prompt-settings-card orchestrator-form-card">
                <div className="orchestrator-form-card-header">
                  <div><div className="card-label">Nodes</div><h3>Session nodes</h3></div>
                  <button className="ghost-button" type="button" onClick={addSession}>Add session</button>
                </div>
                <div className="orchestrator-session-list">
                  {draft.sessions.length === 0 ? <p className="session-control-hint">No sessions yet. Add at least one session before saving.</p> : draft.sessions.map((session, index) => (
                    <article key={session.id} className={`orchestrator-session-row${selectedNodeId === session.id ? " selected" : ""}`} onClick={() => setSelectedNodeId(session.id)}>
                      <div className="orchestrator-session-row-header">
                        <strong>{session.name || `Session ${index + 1}`}</strong>
                        <button className="ghost-button" type="button" onClick={(event) => { event.stopPropagation(); removeSession(session.id); }}>Remove</button>
                      </div>
                      <div className="orchestrator-form-grid">
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`session-id-${session.id}`}>Session id</label>
                          <input id={`session-id-${session.id}`} className="themed-input" type="text" value={session.id} onChange={(event) => setSessionId(session.id, event.target.value)} />
                        </div>
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`session-name-${session.id}`}>Name</label>
                          <input id={`session-name-${session.id}`} className="themed-input" type="text" value={session.name} onChange={(event) => setSessionField(session.id, "name", event.target.value)} />
                        </div>
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`session-agent-${session.id}`}>Agent</label>
                          <select id={`session-agent-${session.id}`} className="themed-input orchestrator-select" value={session.agent} onChange={(event) => setSessionField(session.id, "agent", event.target.value as AgentType)}>
                            {AGENT_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                          </select>
                        </div>
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`session-model-${session.id}`}>Model override</label>
                          <input id={`session-model-${session.id}`} className="themed-input" type="text" value={session.model ?? ""} onChange={(event) => setSessionField(session.id, "model", event.target.value || null)} placeholder="Use the agent default" />
                        </div>
                        <div className="session-control-group orchestrator-toggle-group">
                          <label className="session-control-label" htmlFor={`session-auto-approve-${session.id}`}>Automation</label>
                          <label className="orchestrator-toggle" htmlFor={`session-auto-approve-${session.id}`}>
                            <input id={`session-auto-approve-${session.id}`} type="checkbox" checked={session.autoApprove} onChange={(event) => setSessionField(session.id, "autoApprove", event.target.checked)} />
                            <span>Auto-approve this session's tool calls</span>
                          </label>
                        </div>
                        <div className="session-control-group orchestrator-form-textarea-group">
                          <label className="session-control-label" htmlFor={`session-instructions-${session.id}`}>Instructions</label>
                          <textarea id={`session-instructions-${session.id}`} className="themed-input orchestrator-template-textarea" rows={4} value={session.instructions} onChange={(event) => setSessionField(session.id, "instructions", event.target.value)} />
                        </div>
                      </div>
                    </article>
                  ))}
                </div>
              </article>
              <article className="message-card prompt-settings-card orchestrator-form-card">
                <div className="orchestrator-form-card-header">
                  <div><div className="card-label">Edges</div><h3>Transitions</h3></div>
                  <button className="ghost-button" type="button" onClick={addTransition} disabled={draft.sessions.length < 2}>Add transition</button>
                </div>
                <div className="orchestrator-transition-list">
                  {draft.transitions.length === 0 ? <p className="session-control-hint">No transitions yet. Add one to route completed results into another session's next prompt.</p> : draft.transitions.map((transition) => (
                    <article key={transition.id} className="orchestrator-transition-row">
                      <div className="orchestrator-session-row-header">
                        <strong>{transition.id}</strong>
                        <button className="ghost-button" type="button" onClick={() => setDraft((current) => ({ ...current, transitions: current.transitions.filter((candidate) => candidate.id !== transition.id) }))}>Remove</button>
                      </div>
                      <div className="orchestrator-form-grid">
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`transition-id-${transition.id}`}>Transition id</label>
                          <input id={`transition-id-${transition.id}`} className="themed-input" type="text" value={transition.id} onChange={(event) => setTransitionField(transition.id, "id", event.target.value)} />
                        </div>
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`transition-trigger-${transition.id}`}>Trigger</label>
                          <input id={`transition-trigger-${transition.id}`} className="themed-input" type="text" value="On completion" readOnly />
                        </div>
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`transition-from-${transition.id}`}>From</label>
                          <select id={`transition-from-${transition.id}`} className="themed-input orchestrator-select" value={transition.fromSessionId} onChange={(event) => setTransitionField(transition.id, "fromSessionId", event.target.value)}>
                            {draft.sessions.map((session) => <option key={`from-${session.id}`} value={session.id}>{session.name || session.id}</option>)}
                          </select>
                        </div>
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`transition-to-${transition.id}`}>To</label>
                          <select id={`transition-to-${transition.id}`} className="themed-input orchestrator-select" value={transition.toSessionId} onChange={(event) => setTransitionField(transition.id, "toSessionId", event.target.value)}>
                            {draft.sessions.map((session) => <option key={`to-${session.id}`} value={session.id}>{session.name || session.id}</option>)}
                          </select>
                        </div>
                        <div className="session-control-group">
                          <label className="session-control-label" htmlFor={`transition-result-mode-${transition.id}`}>Result mode</label>
                          <select id={`transition-result-mode-${transition.id}`} className="themed-input orchestrator-select" value={transition.resultMode} onChange={(event) => setTransitionField(transition.id, "resultMode", event.target.value as OrchestratorTransitionResultMode)}>
                            {RESULT_MODE_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                          </select>
                        </div>
                        <div className="session-control-group orchestrator-form-textarea-group">
                          <label className="session-control-label" htmlFor={`transition-prompt-${transition.id}`}>Prompt template</label>
                          <textarea id={`transition-prompt-${transition.id}`} className="themed-input orchestrator-template-textarea" rows={3} value={transition.promptTemplate ?? ""} onChange={(event) => setTransitionField(transition.id, "promptTemplate", event.target.value || null)} placeholder="Use {{result}} to inject the processed source result." />
                        </div>
                      </div>
                    </article>
                  ))}
                </div>
              </article>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

function emptyDraft(): OrchestratorTemplateDraft {
  return { name: "", description: "", sessions: [], transitions: [] };
}

function templateToDraft(template: OrchestratorTemplate): OrchestratorTemplateDraft {
  return {
    name: template.name,
    description: template.description,
    sessions: template.sessions.map((session) => ({
      ...session,
      model: session.model ?? "",
      position: { ...session.position },
    })),
    transitions: template.transitions.map((transition) => ({
      ...transition,
      promptTemplate: transition.promptTemplate ?? "",
    })),
  };
}

function resolveInitialState(
  templates: OrchestratorTemplate[],
  initialTemplateId: string | null,
  restored: PanelState | null,
  startMode: "browse" | "edit" | "new",
): PanelState {
  if (restored) {
    const selectedTemplateId =
      restored.selectedTemplateId && templates.some((template) => template.id === restored.selectedTemplateId)
        ? restored.selectedTemplateId
        : null;
    return finalizePanelState(restored.draft, selectedTemplateId, restored.selectedNodeId);
  }

  if (startMode === "new") {
    return finalizePanelState(emptyDraft(), null, null);
  }

  const selectedTemplate =
    (initialTemplateId
      ? templates.find((template) => template.id === initialTemplateId)
      : null) ??
    templates[0] ??
    null;

  if (!selectedTemplate) {
    return finalizePanelState(emptyDraft(), null, null);
  }

  return finalizePanelState(
    templateToDraft(selectedTemplate),
    selectedTemplate.id,
    selectedTemplate.sessions[0]?.id ?? null,
  );
}

function finalizePanelState(
  draft: OrchestratorTemplateDraft,
  selectedTemplateId: string | null,
  selectedNodeId: string | null,
): PanelState {
  const nextSelectedNodeId =
    selectedNodeId && draft.sessions.some((session) => session.id === selectedNodeId)
      ? selectedNodeId
      : draft.sessions[0]?.id ?? null;

  return {
    draft,
    selectedNodeId: nextSelectedNodeId,
    selectedTemplateId,
  };
}

function readState(stateKey: string): PanelState | null {
  try {
    const raw = window.localStorage.getItem(stateKey);
    if (!raw) {
      return null;
    }
    const parsed = JSON.parse(raw) as Partial<PanelState>;
    if (!parsed.draft || typeof parsed.draft !== "object") {
      return null;
    }

    const draft = parsed.draft as Partial<OrchestratorTemplateDraft>;
    if (
      typeof draft.name !== "string" ||
      typeof draft.description !== "string" ||
      !Array.isArray(draft.sessions) ||
      !Array.isArray(draft.transitions)
    ) {
      return null;
    }

    return finalizePanelState(
      {
        name: draft.name,
        description: draft.description,
        sessions: draft.sessions
          .filter(isSessionTemplate)
          .map((session) => ({
            ...session,
            model: session.model ?? "",
            position: { ...session.position },
          })),
        transitions: draft.transitions
          .filter(isTransitionTemplate)
          .map((transition) => ({
            ...transition,
            promptTemplate: transition.promptTemplate ?? "",
          })),
      },
      typeof parsed.selectedTemplateId === "string" ? parsed.selectedTemplateId : null,
      typeof parsed.selectedNodeId === "string" ? parsed.selectedNodeId : null,
    );
  } catch {
    return null;
  }
}

function isSessionTemplate(value: unknown): value is OrchestratorSessionTemplate {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<OrchestratorSessionTemplate>;
  return (
    typeof candidate.id === "string" &&
    typeof candidate.name === "string" &&
    typeof candidate.agent === "string" &&
    typeof candidate.instructions === "string" &&
    typeof candidate.autoApprove === "boolean" &&
    !!candidate.position &&
    typeof candidate.position.x === "number" &&
    typeof candidate.position.y === "number"
  );
}

function isTransitionTemplate(value: unknown): value is OrchestratorTemplateTransition {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<OrchestratorTemplateTransition>;
  return (
    typeof candidate.id === "string" &&
    typeof candidate.fromSessionId === "string" &&
    typeof candidate.toSessionId === "string" &&
    candidate.trigger === "onCompletion" &&
    (candidate.resultMode === "none" ||
      candidate.resultMode === "lastResponse" ||
      candidate.resultMode === "summary" ||
      candidate.resultMode === "summaryAndLastResponse")
  );
}

function createSession(existingSessions: OrchestratorSessionTemplate[]): OrchestratorSessionTemplate {
  const nextNumber = nextSequenceNumber(existingSessions.map((session) => session.id), "session-");
  const nextX = BOARD_MARGIN + ((existingSessions.length % 4) * 380);
  const nextY = 140 + (Math.floor(existingSessions.length / 4) * 250);

  return {
    id: `session-${nextNumber}`,
    name: `Session ${nextNumber}`,
    agent: "Codex",
    model: "",
    instructions: "",
    autoApprove: false,
    position: clampPosition(nextX, nextY),
  };
}

function createTransition(
  sessions: OrchestratorSessionTemplate[],
  existingTransitions: OrchestratorTemplateTransition[],
): OrchestratorTemplateTransition {
  const nextNumber = nextSequenceNumber(
    existingTransitions.map((transition) => transition.id),
    "transition-",
  );
  const fromSession = sessions[0];
  const toSession = sessions[1] ?? sessions[0];

  return {
    id: `transition-${nextNumber}`,
    fromSessionId: fromSession?.id ?? "",
    toSessionId: toSession?.id ?? "",
    trigger: "onCompletion",
    resultMode: "lastResponse",
    promptTemplate: "Continue with:\n{{result}}",
  };
}

function createTransitionBetween(
  fromSessionId: string,
  toSessionId: string,
  existingTransitions: OrchestratorTemplateTransition[],
): OrchestratorTemplateTransition {
  const nextNumber = nextSequenceNumber(
    existingTransitions.map((transition) => transition.id),
    "transition-",
  );
  return {
    id: `transition-${nextNumber}`,
    fromSessionId,
    toSessionId,
    trigger: "onCompletion",
    resultMode: "lastResponse",
    promptTemplate: "Continue with:\n{{result}}",
  };
}

function nextSequenceNumber(values: string[], prefix: string) {
  let next = 1;
  const seen = new Set(values);
  while (seen.has(`${prefix}${next}`)) {
    next += 1;
  }
  return next;
}

function validateDraft(draft: OrchestratorTemplateDraft) {
  if (!draft.name.trim()) {
    return "Template name cannot be empty.";
  }

  if (draft.sessions.length === 0) {
    return "Add at least one session before saving.";
  }

  const sessionIds = new Set<string>();
  for (const session of draft.sessions) {
    if (!session.id.trim()) {
      return "Session id cannot be empty.";
    }
    if (!session.name.trim()) {
      return "Session name cannot be empty.";
    }
    if (sessionIds.has(session.id.trim())) {
      return `Duplicate session id \`${session.id.trim()}\`.`;
    }
    sessionIds.add(session.id.trim());
    if (!Number.isFinite(session.position.x) || !Number.isFinite(session.position.y)) {
      return `Session \`${session.id.trim()}\` has an invalid canvas position.`;
    }
  }

  const transitionIds = new Set<string>();
  for (const transition of draft.transitions) {
    if (!transition.id.trim()) {
      return "Transition id cannot be empty.";
    }
    if (transitionIds.has(transition.id.trim())) {
      return `Duplicate transition id \`${transition.id.trim()}\`.`;
    }
    transitionIds.add(transition.id.trim());
    if (!sessionIds.has(transition.fromSessionId)) {
      return `Transition \`${transition.id.trim()}\` references an unknown source session.`;
    }
    if (!sessionIds.has(transition.toSessionId)) {
      return `Transition \`${transition.id.trim()}\` references an unknown destination session.`;
    }
    if (transition.fromSessionId === transition.toSessionId) {
      return `Transition \`${transition.id.trim()}\` must connect two different sessions.`;
    }
  }

  return null;
}

function clampPosition(x: number, y: number): OrchestratorNodePosition {
  return {
    x: Math.max(BOARD_MARGIN, Math.min(BOARD_WIDTH - CARD_WIDTH - BOARD_MARGIN, Math.round(x))),
    y: Math.max(BOARD_MARGIN, Math.min(BOARD_HEIGHT - CARD_HEIGHT - BOARD_MARGIN, Math.round(y))),
  };
}

function buildTransitionGeometry(
  transition: OrchestratorTemplateTransition,
  fromNode: OrchestratorSessionTemplate,
  toNode: OrchestratorSessionTemplate,
): TransitionGeometry {
  const startCenter = {
    x: fromNode.position.x + CARD_WIDTH / 2,
    y: fromNode.position.y + CARD_HEIGHT / 2,
  };
  const endCenter = {
    x: toNode.position.x + CARD_WIDTH / 2,
    y: toNode.position.y + CARD_HEIGHT / 2,
  };
  const start = projectCardEdge(startCenter, endCenter);
  const end = projectCardEdge(endCenter, startCenter);
  const midpointX = (start.x + end.x) / 2;
  const midpointY = (start.y + end.y) / 2;
  const dx = end.x - start.x;
  const dy = end.y - start.y;
  const length = Math.hypot(dx, dy) || 1;
  const noteX = midpointX - (dy / length) * 18;
  const noteY = midpointY + (dx / length) * 18;

  return {
    transition,
    path: `M ${start.x} ${start.y} L ${end.x} ${end.y}`,
    midpointX,
    midpointY,
    noteX,
    noteY,
    title: `${transition.id}: ${fromNode.name || fromNode.id} -> ${toNode.name || toNode.id}`,
  };
}

function projectCardEdge(
  from: OrchestratorNodePosition,
  to: OrchestratorNodePosition,
): OrchestratorNodePosition {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  if (dx === 0 && dy === 0) {
    return { ...from };
  }

  const halfWidth = CARD_WIDTH / 2;
  const halfHeight = CARD_HEIGHT / 2;
  const scale = 1 / Math.max(Math.abs(dx) / halfWidth, Math.abs(dy) / halfHeight);

  return {
    x: from.x + dx * scale,
    y: from.y + dy * scale,
  };
}

function anchorPosition(
  session: OrchestratorSessionTemplate,
  side: AnchorSide,
): OrchestratorNodePosition {
  const cx = session.position.x + CARD_WIDTH / 2;
  const cy = session.position.y + CARD_HEIGHT / 2;
  switch (side) {
    case "top":
      return { x: cx, y: session.position.y - CONNECTOR_DOT_OFFSET };
    case "bottom":
      return { x: cx, y: session.position.y + CARD_HEIGHT + CONNECTOR_DOT_OFFSET };
    case "left":
      return { x: session.position.x - CONNECTOR_DOT_OFFSET, y: cy };
    case "right":
      return { x: session.position.x + CARD_WIDTH + CONNECTOR_DOT_OFFSET, y: cy };
  }
}

function TransitionNoteIcon() {
  return (
    <svg
      className="orchestrator-board-transition-note-icon"
      viewBox="0 0 16 16"
      focusable="false"
      aria-hidden="true"
    >
      <path
        d="M4 2.5h5.4L12.5 5.6V13a1 1 0 0 1-1 1H4.5a1 1 0 0 1-1-1v-9a1.5 1.5 0 0 1 1.5-1.5Zm5 .8v2.4h2.4"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M5.4 7.5h5.2M5.4 9.4h5.2M5.4 11.3h3.6"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinecap="round"
      />
    </svg>
  );
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return sanitizeUserFacingErrorMessage(error.message);
  }

  return "Could not load orchestrator templates.";
}
