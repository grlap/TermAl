// Owns marker grouping, ordering, mounted-slot lookup, marker floating-window
// and navigation rendering, and local marker action menus for
// AgentSessionPanel conversations. Does not own marker fetching, mutation
// requests, overview-rail projection, or transcript virtualization.
// Split out of AgentSessionPanel.tsx during the round-39 marker extraction.

import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

import { DiffNavArrow } from "./DiffPanelIcons";
import type { VirtualizedConversationMessageListHandle } from "./VirtualizedConversationMessageList";
import { normalizeConversationMarkerColor } from "../conversation-marker-colors";
import type {
  ConversationMarker,
  CreateConversationMarkerOptions,
  Message,
} from "../types";

type ConversationMarkerContextMenuState = {
  messageId: string;
  clientX: number;
  clientY: number;
  left: number;
  top: number;
  trigger: HTMLElement | null;
  mode: "actions" | "create";
  draftName: string;
};

const NATIVE_MESSAGE_CONTEXT_MENU_SELECTOR = [
  "a[href]",
  "button",
  "input",
  "textarea",
  "select",
  "option",
  "[contenteditable='true']",
  "[contenteditable='']",
  "pre",
  "code",
  "img",
  "picture",
  "video",
  "audio",
  "canvas",
  "svg",
  "[data-native-context-menu='true']",
].join(",");
const CONVERSATION_MARKER_CONTEXT_MENU_TRIGGER_SELECTOR =
  "[data-conversation-marker-menu-trigger='true']";
const CONVERSATION_MARKER_CONTEXT_MENU_VIEWPORT_MARGIN_PX = 8;
// Keep the default label in sync with app-session-actions.ts, and keep the
// UI/server codepoint limit in sync with
// src/session_markers.rs CONVERSATION_MARKER_NAME_MAX_CHARS.
const DEFAULT_CONVERSATION_MARKER_NAME = "Checkpoint";
const CONVERSATION_MARKER_NAME_MAX_LENGTH = 120;

export function groupConversationMarkersByMessageId(
  markers: readonly ConversationMarker[],
) {
  const byMessageId = new Map<string, ConversationMarker[]>();
  markers.forEach((marker) => {
    const bucket = byMessageId.get(marker.messageId);
    if (bucket) {
      bucket.push(marker);
    } else {
      byMessageId.set(marker.messageId, [marker]);
    }
  });
  return byMessageId;
}

export function findMountedConversationMessageSlot(
  messageId: string,
  root: ParentNode = document,
) {
  const expectedItemKey = `message:${messageId}`;
  const candidates = root.querySelectorAll<HTMLElement>(
    "[data-session-search-item-key]",
  );
  for (const candidate of candidates) {
    if (candidate.dataset.sessionSearchItemKey === expectedItemKey) {
      return candidate;
    }
  }
  return null;
}

export function sortConversationMarkersForNavigation(
  markers: readonly ConversationMarker[],
  messages: readonly Message[],
) {
  if (markers.length === 0) {
    return [];
  }
  const messageIndexById = new Map<string, number>();
  messages.forEach((message, index) => {
    messageIndexById.set(message.id, index);
  });
  return [...markers].sort((left, right) => {
    const leftIndex =
      messageIndexById.get(left.messageId) ??
      left.messageIndexHint ??
      Number.MAX_SAFE_INTEGER;
    const rightIndex =
      messageIndexById.get(right.messageId) ??
      right.messageIndexHint ??
      Number.MAX_SAFE_INTEGER;
    if (leftIndex !== rightIndex) {
      return leftIndex - rightIndex;
    }
    const createdOrder = left.createdAt.localeCompare(right.createdAt);
    return createdOrder === 0 ? left.id.localeCompare(right.id) : createdOrder;
  });
}

export function useConversationMarkerJump({
  onConversationSearchItemMount,
  scrollContainerRef,
  sessionId,
  virtualizerHandleRef,
}: {
  onConversationSearchItemMount: (
    itemKey: string,
    node: HTMLElement | null,
  ) => void;
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
  virtualizerHandleRef: RefObject<VirtualizedConversationMessageListHandle | null>;
}) {
  const correctionFrameRef = useRef<number | null>(null);
  const correctionTokenRef = useRef(0);
  const activeSessionIdRef = useRef(sessionId);
  const messageSlotNodesRef = useRef<Map<string, HTMLElement>>(new Map());
  const messageSlotNodesSessionIdRef = useRef(sessionId);
  activeSessionIdRef.current = sessionId;

  const ensureMessageSlotCacheForCurrentSession = useCallback(() => {
    if (messageSlotNodesSessionIdRef.current !== sessionId) {
      messageSlotNodesRef.current = new Map();
      messageSlotNodesSessionIdRef.current = sessionId;
    }
    return messageSlotNodesRef.current;
  }, [sessionId]);

  const cancelCorrectionFrame = useCallback(() => {
    correctionTokenRef.current += 1;
    if (correctionFrameRef.current !== null) {
      window.cancelAnimationFrame(correctionFrameRef.current);
      correctionFrameRef.current = null;
    }
  }, []);

  // Re-creating this ref callback on session changes is intentional: React
  // detaches and re-attaches mounted message slots, repopulating the per-session
  // marker jump cache after the layout-effect reset.
  const handleConversationItemMount = useCallback(
    (itemKey: string, node: HTMLElement | null) => {
      const messageId = itemKey.startsWith("message:")
        ? itemKey.slice("message:".length)
        : null;
      if (messageId) {
        const messageSlotNodes = ensureMessageSlotCacheForCurrentSession();
        if (node) {
          messageSlotNodes.set(messageId, node);
        } else {
          messageSlotNodes.delete(messageId);
        }
      }
      onConversationSearchItemMount(itemKey, node);
    },
    [ensureMessageSlotCacheForCurrentSession, onConversationSearchItemMount],
  );

  const scrollMountedMarkerSlotIntoView = useCallback(
    (messageId: string, behavior: ScrollBehavior = "smooth") => {
      const messageSlotNodes = ensureMessageSlotCacheForCurrentSession();
      const markerSlot =
        messageSlotNodes.get(messageId) ??
        findMountedConversationMessageSlot(
          messageId,
          scrollContainerRef.current ?? document,
        );
      markerSlot?.scrollIntoView?.({ block: "center", behavior });
      return Boolean(markerSlot);
    },
    [ensureMessageSlotCacheForCurrentSession, scrollContainerRef],
  );

  const scheduleCorrectionFrame = useCallback(
    (messageId: string) => {
      cancelCorrectionFrame();
      const correctionToken = correctionTokenRef.current;
      const correctionSessionId = sessionId;
      correctionFrameRef.current = window.requestAnimationFrame(() => {
        correctionFrameRef.current = null;
        if (
          correctionTokenRef.current !== correctionToken ||
          activeSessionIdRef.current !== correctionSessionId
        ) {
          return;
        }
        if (scrollMountedMarkerSlotIntoView(messageId, "auto")) {
          return;
        }
        correctionFrameRef.current = window.requestAnimationFrame(() => {
          correctionFrameRef.current = null;
          if (
            correctionTokenRef.current !== correctionToken ||
            activeSessionIdRef.current !== correctionSessionId
          ) {
            return;
          }
          scrollMountedMarkerSlotIntoView(messageId, "auto");
        });
      });
    },
    [cancelCorrectionFrame, scrollMountedMarkerSlotIntoView, sessionId],
  );

  useEffect(() => cancelCorrectionFrame, [
    cancelCorrectionFrame,
    sessionId,
  ]);

  // Generic message-id jump. The marker-specific entry point below is the same
  // logic with a `marker` wrapper. Both arrow-jump navigation on cards and the
  // marker rail share this helper so they get identical virtualizer-aware
  // correction frames for off-band messages.
  const jumpToMessageId = useCallback(
    (messageId: string) => {
      cancelCorrectionFrame();
      const jumpedWithVirtualizer =
        virtualizerHandleRef.current?.jumpToMessageId(messageId, {
          align: "center",
          flush: true,
        }) ?? false;
      if (jumpedWithVirtualizer) {
        const correctedSynchronously = scrollMountedMarkerSlotIntoView(
          messageId,
          "auto",
        );
        if (!correctedSynchronously) {
          scheduleCorrectionFrame(messageId);
        }
        return;
      }
      scrollMountedMarkerSlotIntoView(messageId);
    },
    [
      cancelCorrectionFrame,
      scheduleCorrectionFrame,
      scrollMountedMarkerSlotIntoView,
      virtualizerHandleRef,
    ],
  );

  const jumpToMarker = useCallback(
    (marker: ConversationMarker) => {
      jumpToMessageId(marker.messageId);
    },
    [jumpToMessageId],
  );

  return {
    handleConversationItemMount,
    jumpToMarker,
    jumpToMessageId,
  };
}

export function MarkerPlusIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M7.25 2.5h1.5v4.75h4.75v1.5H8.75v4.75h-1.5V8.75H2.5v-1.5h4.75Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function MarkerMenuIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M4 8a1.2 1.2 0 1 1-2.4 0A1.2 1.2 0 0 1 4 8Zm5.2 0a1.2 1.2 0 1 1-2.4 0 1.2 1.2 0 0 1 2.4 0Zm5.2 0A1.2 1.2 0 1 1 12 8a1.2 1.2 0 0 1 2.4 0Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function MarkerCloseIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="m4.22 3.16 3.78 3.78 3.78-3.78 1.06 1.06L9.06 8l3.78 3.78-1.06 1.06L8 9.06l-3.78 3.78-1.06-1.06L6.94 8 3.16 4.22Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function ConversationMarkerFloatingWindow({
  markers,
  activeMarkerId,
  onClose,
  onJump,
  onNavigatePrevious,
  onNavigateNext,
}: {
  markers: readonly ConversationMarker[];
  activeMarkerId: string | null;
  onClose: () => void;
  onJump: (marker: ConversationMarker) => void;
  onNavigatePrevious: () => void;
  onNavigateNext: () => void;
}) {
  return (
    <nav
      className="conversation-marker-floating-window"
      aria-label="Conversation markers"
    >
      <div className="conversation-marker-floating-header">
        <div className="conversation-marker-floating-window-copy">
          <span className="card-label">Markers</span>
          <span className="conversation-marker-count">{markers.length}</span>
        </div>
        <div className="conversation-marker-nav-controls">
          <button
            type="button"
            className="ghost-button conversation-marker-nav-button"
            aria-label="Previous marker"
            title="Previous marker"
            disabled={markers.length === 0}
            onClick={onNavigatePrevious}
          >
            <DiffNavArrow direction="up" />
          </button>
          <button
            type="button"
            className="ghost-button conversation-marker-nav-button"
            aria-label="Next marker"
            title="Next marker"
            disabled={markers.length === 0}
            onClick={onNavigateNext}
          >
            <DiffNavArrow direction="down" />
          </button>
          <button
            type="button"
            className="ghost-button conversation-marker-nav-button"
            aria-label="Hide markers window"
            title="Hide markers window"
            onClick={onClose}
          >
            <MarkerCloseIcon />
          </button>
        </div>
      </div>
      {markers.length > 0 ? (
        <div className="conversation-marker-floating-list">
          {markers.map((marker) => (
            <ConversationMarkerChip
              key={marker.id}
              marker={marker}
              isActive={marker.id === activeMarkerId}
              onClick={() => onJump(marker)}
            />
          ))}
        </div>
      ) : (
        <p className="conversation-marker-floating-empty">No markers yet.</p>
      )}
    </nav>
  );
}

export function shouldOpenConversationMarkerContextMenu(
  event: ReactMouseEvent<HTMLElement>,
) {
  const root = event.currentTarget;
  const target = event.target instanceof Element ? event.target : null;
  if (!target) {
    return false;
  }
  if (hasSelectedTextInside(root)) {
    return false;
  }
  // Keep marker actions on an explicit message-header affordance. Message
  // bodies keep the native context menu for copy/select/link/code interactions.
  if (!findConversationMarkerContextMenuTrigger(root, target)) {
    return false;
  }
  return target.closest(NATIVE_MESSAGE_CONTEXT_MENU_SELECTOR) === null;
}

/**
 * Resolves the explicit marker-menu trigger for pointer/keyboard activation.
 *
 * Unlike the context-menu predicate above, this intentionally does not inspect
 * selected text because click/key activation is only allowed from the trigger
 * itself.
 */
export function findConversationMarkerContextMenuTrigger(
  root: HTMLElement,
  target: EventTarget | null,
) {
  if (!(target instanceof Element)) {
    return null;
  }
  const trigger = target.closest<HTMLElement>(
    CONVERSATION_MARKER_CONTEXT_MENU_TRIGGER_SELECTOR,
  );
  return trigger && root.contains(trigger) ? trigger : null;
}

/**
 * Resolves a trigger for synthetic activation, rejecting nested native controls
 * inside the metadata row so buttons/links keep their own click and key
 * behavior.
 */
export function findActivatableConversationMarkerContextMenuTrigger(
  root: HTMLElement,
  target: EventTarget | null,
) {
  const trigger = findConversationMarkerContextMenuTrigger(root, target);
  if (!trigger || !(target instanceof Element)) {
    return null;
  }
  const nativeTarget = target.closest(NATIVE_MESSAGE_CONTEXT_MENU_SELECTOR);
  return nativeTarget && nativeTarget !== trigger && trigger.contains(nativeTarget)
    ? null
    : trigger;
}

export function useConversationMarkerContextMenu({
  isActive,
  isMarkerPanelVisible,
  markersByMessageId,
  onCreateConversationMarker,
  onDeleteConversationMarker,
  onSetMarkerPanelVisible,
  scrollContainerRef,
  sessionId,
  visibleMessageIds,
}: {
  isActive: boolean;
  isMarkerPanelVisible: boolean;
  markersByMessageId: ReadonlyMap<string, readonly ConversationMarker[]>;
  onCreateConversationMarker: (
    sessionId: string,
    messageId: string,
    options?: CreateConversationMarkerOptions,
  ) => void;
  onDeleteConversationMarker: (sessionId: string, markerId: string) => void;
  onSetMarkerPanelVisible: (isVisible: boolean) => void;
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
  visibleMessageIds: ReadonlySet<string>;
}) {
  const [contextMenu, setContextMenu] =
    useState<ConversationMarkerContextMenuState | null>(null);
  const contextMenuRef = useRef<ConversationMarkerContextMenuState | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const firstMenuItemRef = useRef<HTMLButtonElement | null>(null);
  const createNameInputRef = useRef<HTMLInputElement | null>(null);
  const createNameLimitId = useId();
  const focusRestoreFrameRef = useRef<number | null>(null);
  const isContextMenuOpen = contextMenu !== null;
  contextMenuRef.current = contextMenu;

  const cancelFocusRestoreFrame = useCallback(() => {
    if (focusRestoreFrameRef.current !== null) {
      window.cancelAnimationFrame(focusRestoreFrameRef.current);
      focusRestoreFrameRef.current = null;
    }
  }, []);

  const closeContextMenu = useCallback(
    ({ restoreFocus = false }: { restoreFocus?: boolean } = {}) => {
      const trigger = contextMenuRef.current?.trigger ?? null;
      cancelFocusRestoreFrame();
      setContextMenu(null);
      if (restoreFocus && trigger) {
        focusRestoreFrameRef.current = window.requestAnimationFrame(() => {
          focusRestoreFrameRef.current = null;
          if (trigger.isConnected) {
            focusConversationMarkerContextMenuTrigger(trigger);
          }
        });
      }
    },
    [cancelFocusRestoreFrame],
  );

  const openContextMenu = useCallback(
    ({
      clientX,
      clientY,
      messageId,
      trigger,
    }: {
      clientX: number;
      clientY: number;
      messageId: string;
      trigger: HTMLElement | null;
    }) => {
      setContextMenu({
        clientX,
        clientY,
        left: clientX,
        top: clientY,
        messageId,
        trigger,
        mode: "actions",
        draftName: DEFAULT_CONVERSATION_MARKER_NAME,
      });
    },
    [],
  );

  useEffect(() => {
    closeContextMenu();
  }, [closeContextMenu, sessionId]);

  useEffect(() => cancelFocusRestoreFrame, [cancelFocusRestoreFrame]);

  useEffect(() => {
    if (!isActive) {
      closeContextMenu();
    }
  }, [closeContextMenu, isActive]);

  useEffect(() => {
    if (
      contextMenu &&
      !visibleMessageIds.has(contextMenu.messageId)
    ) {
      closeContextMenu();
    }
  }, [closeContextMenu, contextMenu, visibleMessageIds]);

  useEffect(() => {
    if (!isContextMenuOpen) {
      return;
    }
    const frameId = window.requestAnimationFrame(() => {
      if (contextMenu?.mode === "create") {
        createNameInputRef.current?.focus();
        createNameInputRef.current?.select();
        return;
      }
      firstMenuItemRef.current?.focus();
    });
    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [isContextMenuOpen, contextMenu?.messageId, contextMenu?.mode]);

  useLayoutEffect(() => {
    if (!contextMenu || !menuRef.current) {
      return;
    }
    const nextPosition = clampConversationMarkerContextMenuPosition(
      contextMenu.clientX,
      contextMenu.clientY,
      menuRef.current,
    );
    if (
      nextPosition.left !== contextMenu.left ||
      nextPosition.top !== contextMenu.top
    ) {
      setContextMenu({
        ...contextMenu,
        ...nextPosition,
      });
    }
  }, [contextMenu]);

  useEffect(() => {
    if (!isContextMenuOpen) {
      return;
    }

    const handlePointerDown = (event: PointerEvent) => {
      if (
        event.target instanceof Element &&
        event.target.closest(".conversation-marker-context-menu")
      ) {
        return;
      }
      closeContextMenu();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (
        event.target instanceof Element &&
        event.target.closest(".conversation-marker-context-menu")
      ) {
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        closeContextMenu({ restoreFocus: true });
      }
    };
    const handleScroll = (event: Event) => {
      const scrollRoot = scrollContainerRef.current;
      if (!scrollRoot) {
        closeContextMenu();
        return;
      }
      const target = event.target;
      if (target instanceof Node && scrollRoot.contains(target)) {
        closeContextMenu();
      }
    };
    const handleViewportMove = () => {
      if (contextMenuRef.current?.mode === "create") {
        const menu = menuRef.current;
        if (!menu) {
          return;
        }
        setContextMenu((current) => {
          if (!current || current.mode !== "create") {
            return current;
          }
          const nextPosition = clampConversationMarkerContextMenuPosition(
            current.clientX,
            current.clientY,
            menu,
          );
          return nextPosition.left !== current.left ||
            nextPosition.top !== current.top
            ? { ...current, ...nextPosition }
            : current;
        });
        return;
      }
      closeContextMenu();
    };

    document.addEventListener("pointerdown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    document.addEventListener("scroll", handleScroll, true);
    window.addEventListener("scroll", handleViewportMove);
    window.addEventListener("resize", handleViewportMove);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
      document.removeEventListener("scroll", handleScroll, true);
      window.removeEventListener("scroll", handleViewportMove);
      window.removeEventListener("resize", handleViewportMove);
    };
  }, [closeContextMenu, isContextMenuOpen, scrollContainerRef]);

  const contextMenuMarkers = contextMenu
    ? markersByMessageId.get(contextMenu.messageId) ?? []
    : [];
  const createMarkerName = contextMenu?.draftName.trim() ?? "";
  const createMarkerNameLength = contextMenu
    ? Array.from(contextMenu.draftName).length
    : 0;
  const updateCreateMarkerName = (name: string) => {
    setContextMenu((current) =>
      current
        ? {
            ...current,
            draftName: Array.from(name)
              .slice(0, CONVERSATION_MARKER_NAME_MAX_LENGTH)
              .join(""),
          }
        : current,
    );
  };
  const showCreateMarkerForm = () => {
    setContextMenu((current) =>
      current
        ? {
            ...current,
            mode: "create",
            draftName: current.draftName || DEFAULT_CONVERSATION_MARKER_NAME,
          }
        : current,
    );
  };
  const submitCreateMarker = () => {
    if (!contextMenu || !createMarkerName) {
      return;
    }
    onCreateConversationMarker(sessionId, contextMenu.messageId, {
      name: createMarkerName,
    });
    closeContextMenu({ restoreFocus: true });
  };
  const contextMenuNode = contextMenu
    ? createPortal(
        <div
          ref={menuRef}
          className="conversation-marker-context-menu"
          role={contextMenu.mode === "create" ? "dialog" : "menu"}
          aria-label={
            contextMenu.mode === "create"
              ? "Create conversation marker"
              : "Conversation marker actions"
          }
          style={{
            left: contextMenu.left,
            top: contextMenu.top,
          }}
          onContextMenu={(event) => event.preventDefault()}
          onKeyDown={(event) =>
            handleConversationMarkerContextMenuKeyDown(
              event,
              menuRef.current,
              () => closeContextMenu({ restoreFocus: true }),
            )
          }
        >
          {contextMenu.mode === "create" ? (
            <form
              className="conversation-marker-context-menu-form"
              onSubmit={(event) => {
                event.preventDefault();
                submitCreateMarker();
              }}
            >
              <label className="conversation-marker-context-menu-label">
                <span>Marker label</span>
                <input
                  ref={createNameInputRef}
                  className="conversation-marker-context-menu-input"
                  value={contextMenu.draftName}
                  onChange={(event) => updateCreateMarkerName(event.target.value)}
                  aria-describedby={createNameLimitId}
                />
              </label>
              <span
                id={createNameLimitId}
                className="conversation-marker-context-menu-limit"
                aria-live="polite"
              >
                {createMarkerNameLength}/{CONVERSATION_MARKER_NAME_MAX_LENGTH}{" "}
                characters
                {createMarkerNameLength === CONVERSATION_MARKER_NAME_MAX_LENGTH
                  ? " maximum"
                  : ""}
              </span>
              <div className="conversation-marker-context-menu-form-actions">
                <button
                  type="submit"
                  className="conversation-marker-context-menu-action"
                  disabled={!createMarkerName}
                >
                  Create marker
                </button>
                <button
                  type="button"
                  className="conversation-marker-context-menu-action"
                  onClick={() => closeContextMenu({ restoreFocus: true })}
                >
                  Cancel
                </button>
              </div>
            </form>
          ) : (
            <>
              <button
                ref={firstMenuItemRef}
                type="button"
                role="menuitem"
                className="conversation-marker-context-menu-item"
                onClick={showCreateMarkerForm}
              >
                Add checkpoint marker
              </button>
              {contextMenuMarkers.length > 0 ? (
                <>
                  <div className="conversation-marker-context-menu-separator" role="separator" />
                  {contextMenuMarkers.map((marker) => (
                    <button
                      key={marker.id}
                      type="button"
                      role="menuitem"
                      className="conversation-marker-context-menu-item conversation-marker-context-menu-item-danger"
                      onClick={() => {
                        onDeleteConversationMarker(sessionId, marker.id);
                        closeContextMenu();
                      }}
                    >
                      Remove {marker.name || "marker"}
                    </button>
                  ))}
                </>
              ) : null}
              <div className="conversation-marker-context-menu-separator" role="separator" />
              <button
                type="button"
                role="menuitem"
                className="conversation-marker-context-menu-item"
                onClick={() => {
                  onSetMarkerPanelVisible(!isMarkerPanelVisible);
                  closeContextMenu({ restoreFocus: true });
                }}
              >
                {isMarkerPanelVisible ? "Hide markers window" : "Show markers window"}
              </button>
            </>
          )}
        </div>,
      document.body,
    )
    : null;

  return {
    contextMenuNode,
    contextMenuMessageId: contextMenu?.messageId ?? null,
    isContextMenuOpen,
    openContextMenu,
  };
}

function ConversationMarkerChip({
  marker,
  isActive,
  onClick,
}: {
  marker: ConversationMarker;
  isActive: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={`conversation-marker-chip${isActive ? " is-active" : ""}`}
      style={{
        "--conversation-marker-color": normalizeConversationMarkerColor(
          marker.color,
        ),
      } as CSSProperties}
      title={marker.body ?? marker.name}
      aria-label={`Jump to ${formatConversationMarkerKind(marker.kind)} marker ${marker.name}`}
      onClick={onClick}
    >
      <span className="conversation-marker-chip-swatch" aria-hidden="true" />
      <span className="conversation-marker-chip-name">{marker.name}</span>
      <span className="conversation-marker-chip-kind">
        {formatConversationMarkerKind(marker.kind)}
      </span>
    </button>
  );
}

function formatConversationMarkerKind(kind: ConversationMarker["kind"]) {
  switch (kind) {
    case "checkpoint":
      return "Checkpoint";
    case "decision":
      return "Decision";
    case "review":
      return "Review";
    case "bug":
      return "Bug";
    case "question":
      return "Question";
    case "handoff":
      return "Handoff";
    case "custom":
    default:
      return "Marker";
  }
}

function hasSelectedTextInside(root: HTMLElement) {
  const selection = window.getSelection?.();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) {
    return false;
  }
  if (!selection.toString().trim()) {
    return false;
  }
  for (let index = 0; index < selection.rangeCount; index += 1) {
    const range = selection.getRangeAt(index);
    const ancestor = range.commonAncestorContainer;
    const ancestorElement =
      ancestor instanceof Element ? ancestor : ancestor.parentElement;
    if (ancestorElement && root.contains(ancestorElement)) {
      return true;
    }
  }
  return false;
}

function handleConversationMarkerContextMenuKeyDown(
  event: ReactKeyboardEvent<HTMLDivElement>,
  menu: HTMLDivElement | null,
  closeMenu: () => void,
) {
  if (
    event.target instanceof HTMLElement &&
    event.target.closest(".conversation-marker-context-menu-form") &&
    event.key !== "Escape"
  ) {
    return;
  }
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    closeMenu();
    return;
  }
  const menuItems = getConversationMarkerContextMenuItems(menu);
  if (menuItems.length === 0) {
    return;
  }
  const currentIndex = menuItems.findIndex(
    (item) => item === document.activeElement,
  );
  let nextIndex: number | null = null;
  switch (event.key) {
    case "ArrowDown":
    case "ArrowRight":
      nextIndex =
        currentIndex === -1 ? 0 : (currentIndex + 1) % menuItems.length;
      break;
    case "ArrowUp":
    case "ArrowLeft":
      nextIndex =
        currentIndex === -1
          ? menuItems.length - 1
          : (currentIndex - 1 + menuItems.length) % menuItems.length;
      break;
    case "Home":
      nextIndex = 0;
      break;
    case "End":
      nextIndex = menuItems.length - 1;
      break;
  }
  if (nextIndex === null) {
    return;
  }
  event.preventDefault();
  menuItems[nextIndex]?.focus();
}

function focusConversationMarkerContextMenuTrigger(trigger: HTMLElement) {
  const hadTabIndex = trigger.hasAttribute("tabindex");
  // Custom renderers may mark a header as a marker trigger without making it
  // keyboard-focusable. Temporarily opt it into programmatic focus so closing
  // the menu still restores focus to the exact trigger that opened it.
  if (!hadTabIndex) {
    trigger.tabIndex = -1;
  }
  trigger.focus({ preventScroll: true });
  if (!hadTabIndex) {
    trigger.removeAttribute("tabindex");
  }
}

function clampConversationMarkerContextMenuPosition(
  clientX: number,
  clientY: number,
  menu: HTMLElement,
) {
  const rect = menu.getBoundingClientRect();
  const viewportWidth =
    window.innerWidth || document.documentElement.clientWidth;
  const viewportHeight =
    window.innerHeight || document.documentElement.clientHeight;
  const menuWidth = rect.width || menu.offsetWidth;
  const menuHeight = rect.height || menu.offsetHeight;
  const maxLeft = Math.max(
    CONVERSATION_MARKER_CONTEXT_MENU_VIEWPORT_MARGIN_PX,
    viewportWidth - menuWidth - CONVERSATION_MARKER_CONTEXT_MENU_VIEWPORT_MARGIN_PX,
  );
  const maxTop = Math.max(
    CONVERSATION_MARKER_CONTEXT_MENU_VIEWPORT_MARGIN_PX,
    viewportHeight - menuHeight - CONVERSATION_MARKER_CONTEXT_MENU_VIEWPORT_MARGIN_PX,
  );

  return {
    left: Math.min(
      Math.max(clientX, CONVERSATION_MARKER_CONTEXT_MENU_VIEWPORT_MARGIN_PX),
      maxLeft,
    ),
    top: Math.min(
      Math.max(clientY, CONVERSATION_MARKER_CONTEXT_MENU_VIEWPORT_MARGIN_PX),
      maxTop,
    ),
  };
}

function getConversationMarkerContextMenuItems(menu: HTMLDivElement | null) {
  return Array.from(
    menu?.querySelectorAll<HTMLButtonElement>(
      '[role="menuitem"]:not(:disabled)',
    ) ?? [],
  );
}
