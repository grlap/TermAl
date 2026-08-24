import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  createResponseBoardTab,
  createResponseBoardCard,
  deleteResponseBoardTab,
  deleteResponseBoardCard,
  fetchResponseBoard,
  fetchResponseBoardTab,
  fetchResponseBoardTabs,
  reorderResponseBoardTabs,
  stageResponseBoardCard,
  updateResponseBoardCard,
  updateResponseBoardTab,
  type ResponseBoardCard,
  type ResponseBoardTab,
} from "../api";
import { getErrorMessage } from "../app-utils";
import { MessageCard } from "../message-cards";
import {
  createResponseBoardInvalidationSource,
  nextResponseBoardCardPosition,
  notifyResponseBoardChanged,
  readResponseBoardMessageDragData,
  subscribeResponseBoardInvalidation,
} from "../response-board";
import type { WorkspaceResponseBoardView } from "../workspace";
import {
  clampBoardZoom,
  fitResponseBoardCardsInView,
  readStoredBoardZoom,
  RESPONSE_BOARD_ZOOM_STORAGE_KEY,
  responseBoardViewShowsAnyCard,
  wheelRequestsBoardZoom,
  zoomBoardViewAtPoint,
} from "./response-board-camera";
import {
  RESPONSE_BOARD_STAGED_CARD_MIME,
  ResponseBoardPreview,
  ResponseBoardStagingTray,
} from "./ResponseBoardStagingTray";
import { ResponseBoardTabStrip } from "./ResponseBoardTabStrip";
import { useCommittedRef } from "./use-committed-ref";

export {
  fitResponseBoardCardsInView,
  RESPONSE_BOARD_ZOOM_STORAGE_KEY,
  responseBoardViewShowsAnyCard,
} from "./response-board-camera";

const BOARD_PADDING = 72;
const CARD_PATCH_DEBOUNCE_MS = 250;
const BOARD_ZOOM_BUTTON_FACTOR = 1.2;
const BOARD_WHEEL_ZOOM_SENSITIVITY = 0.0015;
type BoardView = WorkspaceResponseBoardView;

type CardGesture = {
  kind: "move" | "resize";
  cardId: string;
  pointerId: number;
  clientX: number;
  clientY: number;
  x: number;
  y: number;
  w: number;
  h: number;
  zoom: number;
};

type PanGesture = {
  pointerId: number;
  clientX: number;
  clientY: number;
  panX: number;
  panY: number;
};

type PendingCameraRepair = {
  cards: ResponseBoardCard[];
  tabId: string;
};

export function ResponseBoardPanel({
  refreshToken,
  workspaceTabId = "",
  activeBoardTabId = null,
  boardViews = {},
  onWorkspaceStateChange = () => {},
  onOpenSource,
}: {
  refreshToken: string;
  workspaceTabId?: string;
  activeBoardTabId?: string | null;
  boardViews?: Record<string, WorkspaceResponseBoardView>;
  onWorkspaceStateChange?: (
    workspaceTabId: string,
    activeBoardTabId: string,
    view: WorkspaceResponseBoardView,
    knownBoardTabIds?: readonly string[],
  ) => void;
  onOpenSource: (card: ResponseBoardCard) => void;
}) {
  const usesPartitionedBoard = workspaceTabId.length > 0;
  const [tabs, setTabs] = useState<ResponseBoardTab[]>([]);
  const [selectedTabId, setSelectedTabId] = useState<string | null>(
    activeBoardTabId,
  );
  const [cards, setCards] = useState<ResponseBoardCard[]>([]);
  const [stagedCards, setStagedCards] = useState<ResponseBoardCard[]>([]);
  const [previewCardId, setPreviewCardId] = useState<string | null>(null);
  const [newTabName, setNewTabName] = useState("");
  const [isAddingTab, setIsAddingTab] = useState(false);
  const [renamingTabId, setRenamingTabId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [isReorderingTabs, setIsReorderingTabs] = useState(false);
  const [invalidationSource] = useState(
    createResponseBoardInvalidationSource,
  );
  const [invalidationRevision, setInvalidationRevision] = useState(0);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<BoardView>(() => {
    const stored = activeBoardTabId ? boardViews[activeBoardTabId] : null;
    return (
      stored ?? {
        panX: 0,
        panY: 0,
        zoom: usesPartitionedBoard ? 1 : readStoredBoardZoom(),
      }
    );
  });
  const [isPanning, setIsPanning] = useState(false);
  const [pendingCameraRepair, setPendingCameraRepair] =
    useState<PendingCameraRepair | null>(null);
  const cardsRef = useRef<ResponseBoardCard[]>([]);
  const stagedCardsRef = useRef<ResponseBoardCard[]>([]);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const cardGestureRef = useRef<CardGesture | null>(null);
  const panGestureRef = useRef<PanGesture | null>(null);
  const patchTimersRef = useRef(new Map<string, number>());
  const patchRequestsRef = useRef(new Map<string, number>());
  const patchVersionsRef = useRef(new Map<string, number>());
  const visibleTabIdRef = useRef<string | null>(null);
  const viewPersistTimerRef = useRef<number | null>(null);
  const pendingViewPersistRef = useRef<{
    workspaceTabId: string;
    tabId: string;
    view: BoardView;
  } | null>(null);
  const tabReorderPendingRef = useRef(false);
  const isMountedRef = useRef(true);
  const isUnmountingRef = useRef(false);
  const tabButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  const boardViewsRef = useCommittedRef(boardViews);
  const activeBoardTabIdRef = useCommittedRef(activeBoardTabId);
  const selectedTabIdRef = useCommittedRef(selectedTabId);
  const viewRef = useCommittedRef(view);
  const onWorkspaceStateChangeRef = useCommittedRef(onWorkspaceStateChange);
  const replaceCards = useCallback((nextCards: ResponseBoardCard[]) => {
    cardsRef.current = nextCards;
    setCards(nextCards);
  }, []);
  const replaceStagedCards = useCallback(
    (nextCards: ResponseBoardCard[]) => {
      stagedCardsRef.current = nextCards;
      setStagedCards(nextCards);
    },
    [],
  );

  const refreshTabs = useCallback(async () => {
    const response = await fetchResponseBoardTabs();
    return response.tabs;
  }, []);

  const notifySiblingBoards = useCallback(
    () => notifyResponseBoardChanged(invalidationSource),
    [invalidationSource],
  );

  useEffect(
    () =>
      subscribeResponseBoardInvalidation((source) => {
        if (source !== invalidationSource) {
          setInvalidationRevision((current) => current + 1);
        }
      }),
    [invalidationSource],
  );

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  useLayoutEffect(() => {
    isUnmountingRef.current = false;
    return () => {
      // Layout cleanup precedes passive-effect cleanup, allowing the camera
      // debounce below to distinguish an outer panel unmount from an ordinary
      // view update without flushing on every pointer-move render.
      isUnmountingRef.current = true;
    };
  }, []);

  useEffect(() => {
    let active = true;
    setError(null);
    if (!usesPartitionedBoard) {
      setIsLoading(true);
      void fetchResponseBoard().then(
        (board) => {
          if (!active) {
            return;
          }
          replaceCards(board.cards);
          replaceStagedCards([]);
          setIsLoading(false);
        },
        (reason) => {
          if (!active) {
            return;
          }
          setError(getErrorMessage(reason));
          setIsLoading(false);
        },
      );
      return () => {
        active = false;
      };
    }
    void refreshTabs().then(
      (nextTabs) => {
        if (!active) {
          return;
        }
        setTabs(nextTabs);
        const current = selectedTabIdRef.current;
        const requested = activeBoardTabIdRef.current;
        const nextSelectedTabId =
          (current && nextTabs.some((tab) => tab.id === current)
            ? current
            : requested && nextTabs.some((tab) => tab.id === requested)
              ? requested
              : nextTabs[0]?.id) ?? null;
        setSelectedTabId(nextSelectedTabId);
        if (nextSelectedTabId) {
          const nextWorkspaceView =
            nextSelectedTabId === selectedTabIdRef.current
              ? viewRef.current
              : (boardViewsRef.current[nextSelectedTabId] ?? {
                  panX: 0,
                  panY: 0,
                  zoom: 1,
                });
          onWorkspaceStateChangeRef.current(
            workspaceTabId,
            nextSelectedTabId,
            nextWorkspaceView,
            nextTabs.map((tab) => tab.id),
          );
        }
        if (nextTabs.length === 0) {
          visibleTabIdRef.current = null;
          replaceCards([]);
          replaceStagedCards([]);
          setIsLoading(false);
        }
      },
      (reason) => {
        if (!active) {
          return;
        }
        setError(getErrorMessage(reason));
        setIsLoading(false);
      },
    );
    return () => {
      active = false;
    };
  }, [
    activeBoardTabIdRef,
    boardViewsRef,
    invalidationRevision,
    onWorkspaceStateChangeRef,
    refreshTabs,
    refreshToken,
    replaceCards,
    replaceStagedCards,
    selectedTabIdRef,
    usesPartitionedBoard,
    viewRef,
    workspaceTabId,
  ]);

  useEffect(() => {
    if (activeBoardTabId) {
      setSelectedTabId(activeBoardTabId);
    }
  }, [activeBoardTabId]);

  useEffect(() => {
    if (!usesPartitionedBoard || !selectedTabId) {
      return;
    }
    setView(
      boardViewsRef.current[selectedTabId] ?? { panX: 0, panY: 0, zoom: 1 },
    );
  }, [boardViewsRef, selectedTabId, usesPartitionedBoard]);

  useEffect(() => {
    if (!usesPartitionedBoard || !selectedTabId) {
      return;
    }
    let active = true;
    const refreshInPlace = visibleTabIdRef.current === selectedTabId;
    if (!refreshInPlace) {
      setIsLoading(true);
      replaceCards([]);
      replaceStagedCards([]);
      setPreviewCardId(null);
    }
    setError(null);
    void fetchResponseBoardTab(selectedTabId).then(
      (tabView) => {
        if (!active) {
          return;
        }
        const localCardsById = new Map(
          cardsRef.current.map((card) => [card.id, card]),
        );
        const nextCards = refreshInPlace
          ? tabView.cards.map((card) => {
              const localCard = localCardsById.get(card.id);
              const keepsLocalGeometry =
                cardGestureRef.current?.cardId === card.id ||
                patchTimersRef.current.has(card.id) ||
                patchRequestsRef.current.has(card.id);
              return localCard && keepsLocalGeometry
                ? {
                    ...card,
                    x: localCard.x,
                    y: localCard.y,
                    w: localCard.w,
                    h: localCard.h,
                  }
                : card;
            })
          : tabView.cards;
        visibleTabIdRef.current = selectedTabId;
        replaceCards(nextCards);
        replaceStagedCards(tabView.stagedCards);
        if (!refreshInPlace) {
          setPendingCameraRepair(
            nextCards.length > 0
              ? { cards: nextCards, tabId: selectedTabId }
              : null,
          );
        }
        setPreviewCardId((current) =>
          tabView.stagedCards.some((card) => card.id === current)
            ? current
            : null,
        );
        if (!refreshInPlace) {
          setIsLoading(false);
        }
      },
      (reason) => {
        if (!active) {
          return;
        }
        setError(getErrorMessage(reason));
        if (!refreshInPlace) {
          setIsLoading(false);
        }
      },
    );
    return () => {
      active = false;
    };
  }, [
    refreshToken,
    invalidationRevision,
    replaceCards,
    replaceStagedCards,
    selectedTabId,
    usesPartitionedBoard,
  ]);

  useEffect(() => {
    const surface = surfaceRef.current;
    if (
      !surface ||
      !pendingCameraRepair ||
      pendingCameraRepair.tabId !== selectedTabId
    ) {
      return;
    }

    let frameId: number | null = null;
    let resizeObserver: ResizeObserver | null = null;
    const repairCameraOnceSized = () => {
      const bounds = surface.getBoundingClientRect();
      if (bounds.width <= 0 || bounds.height <= 0) {
        return false;
      }
      setView((current) => {
        if (
          responseBoardViewShowsAnyCard(
            current,
            pendingCameraRepair.cards,
            bounds.width,
            bounds.height,
          )
        ) {
          return current;
        }
        return (
          fitResponseBoardCardsInView(
            pendingCameraRepair.cards,
            bounds.width,
            bounds.height,
          ) ?? current
        );
      });
      setPendingCameraRepair(null);
      return true;
    };

    if (repairCameraOnceSized()) {
      return;
    }
    frameId = window.requestAnimationFrame(() => {
      if (!repairCameraOnceSized() && typeof ResizeObserver !== "undefined") {
        resizeObserver = new ResizeObserver(() => {
          if (repairCameraOnceSized()) {
            resizeObserver?.disconnect();
            resizeObserver = null;
          }
        });
        resizeObserver.observe(surface);
      }
    });
    return () => {
      if (frameId !== null) {
        window.cancelAnimationFrame(frameId);
      }
      resizeObserver?.disconnect();
    };
  }, [pendingCameraRepair, selectedTabId]);

  useEffect(() => {
    if (!usesPartitionedBoard || !selectedTabId) {
      return;
    }
    if (viewPersistTimerRef.current !== null) {
      window.clearTimeout(viewPersistTimerRef.current);
    }
    const pending = { workspaceTabId, tabId: selectedTabId, view };
    pendingViewPersistRef.current = pending;
    viewPersistTimerRef.current = window.setTimeout(() => {
      viewPersistTimerRef.current = null;
      if (pendingViewPersistRef.current !== pending) {
        return;
      }
      pendingViewPersistRef.current = null;
      onWorkspaceStateChangeRef.current(
        pending.workspaceTabId,
        pending.tabId,
        pending.view,
      );
    }, 160);
    return () => {
      if (viewPersistTimerRef.current !== null) {
        window.clearTimeout(viewPersistTimerRef.current);
        viewPersistTimerRef.current = null;
      }
      if (
        pendingViewPersistRef.current === pending &&
        (selectedTabIdRef.current !== pending.tabId || isUnmountingRef.current)
      ) {
        pendingViewPersistRef.current = null;
        onWorkspaceStateChangeRef.current(
          pending.workspaceTabId,
          pending.tabId,
          pending.view,
        );
      }
    };
  }, [
    onWorkspaceStateChangeRef,
    selectedTabId,
    selectedTabIdRef,
    usesPartitionedBoard,
    view,
    workspaceTabId,
  ]);

  useEffect(
    () => () => {
      for (const timer of patchTimersRef.current.values()) {
        window.clearTimeout(timer);
      }
      patchTimersRef.current.clear();
      patchRequestsRef.current.clear();
    },
    [],
  );

  useEffect(() => {
    if (usesPartitionedBoard) {
      return;
    }
    try {
      window.localStorage.setItem(
        RESPONSE_BOARD_ZOOM_STORAGE_KEY,
        String(view.zoom),
      );
    } catch {
      // View persistence is best-effort; board data remains server-owned.
    }
  }, [usesPartitionedBoard, view.zoom]);

  const scheduleCardPatch = useCallback((card: ResponseBoardCard) => {
    const currentTimer = patchTimersRef.current.get(card.id);
    if (currentTimer !== undefined) {
      window.clearTimeout(currentTimer);
    }
    const version = (patchVersionsRef.current.get(card.id) ?? 0) + 1;
    patchVersionsRef.current.set(card.id, version);
    const timer = window.setTimeout(() => {
      patchTimersRef.current.delete(card.id);
      patchRequestsRef.current.set(card.id, version);
      const finishRequest = () => {
        if (patchRequestsRef.current.get(card.id) === version) {
          patchRequestsRef.current.delete(card.id);
        }
      };
      void updateResponseBoardCard(card.id, {
        x: card.x,
        y: card.y,
        w: card.w,
        h: card.h,
      }).then(
        (persisted) => {
          finishRequest();
          notifySiblingBoards();
          if (
            !isMountedRef.current ||
            patchVersionsRef.current.get(card.id) !== version
          ) {
            return;
          }
          replaceCards(
            cardsRef.current.map((candidate) =>
              candidate.id === persisted.id ? persisted : candidate,
            ),
          );
        },
        (reason) => {
          finishRequest();
          if (
            isMountedRef.current &&
            patchVersionsRef.current.get(card.id) === version
          ) {
            setError(getErrorMessage(reason));
          }
        },
      );
    }, CARD_PATCH_DEBOUNCE_MS);
    patchTimersRef.current.set(card.id, timer);
  }, [notifySiblingBoards, replaceCards]);

  const updateCardGeometry = useCallback(
    (cardId: string, update: (card: ResponseBoardCard) => ResponseBoardCard) => {
      const currentCard = cardsRef.current.find((card) => card.id === cardId);
      if (!currentCard) {
        return;
      }
      const changedCard = update(currentCard);
      replaceCards(
        cardsRef.current.map((card) =>
          card.id === cardId ? changedCard : card,
        ),
      );
      scheduleCardPatch(changedCard);
    },
    [replaceCards, scheduleCardPatch],
  );

  const handleCardPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLElement>) => {
      const gesture = cardGestureRef.current;
      if (!gesture || gesture.pointerId !== event.pointerId) {
        return;
      }
      const deltaX = (event.clientX - gesture.clientX) / gesture.zoom;
      const deltaY = (event.clientY - gesture.clientY) / gesture.zoom;
      updateCardGeometry(gesture.cardId, (card) =>
        gesture.kind === "move"
          ? {
              ...card,
              x: Math.max(0, gesture.x + deltaX),
              y: Math.max(0, gesture.y + deltaY),
            }
          : {
              ...card,
              w: Math.min(1_600, Math.max(240, gesture.w + deltaX)),
              h: Math.min(1_600, Math.max(160, gesture.h + deltaY)),
            },
      );
    },
    [updateCardGeometry],
  );

  const finishCardGesture = useCallback((event: ReactPointerEvent<HTMLElement>) => {
    if (cardGestureRef.current?.pointerId !== event.pointerId) {
      return;
    }
    cardGestureRef.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
  }, []);

  const beginCardGesture = useCallback(
    (
      event: ReactPointerEvent<HTMLElement>,
      card: ResponseBoardCard,
      kind: CardGesture["kind"],
    ) => {
      if (event.button !== 0) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      const cardNode = event.currentTarget.closest(
        ".response-board-card",
      ) as HTMLElement | null;
      cardNode?.setPointerCapture?.(event.pointerId);
      cardGestureRef.current = {
        kind,
        cardId: card.id,
        pointerId: event.pointerId,
        clientX: event.clientX,
        clientY: event.clientY,
        x: card.x,
        y: card.y,
        w: card.w,
        h: card.h,
        zoom: view.zoom,
      };
    },
    [view.zoom],
  );

  const handleRemove = useCallback((cardId: string) => {
    const timer = patchTimersRef.current.get(cardId);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      patchTimersRef.current.delete(cardId);
    }
    patchVersionsRef.current.set(cardId, (patchVersionsRef.current.get(cardId) ?? 0) + 1);
    void (async () => {
      try {
        await deleteResponseBoardCard(cardId);
        notifySiblingBoards();
        if (!isMountedRef.current) {
          return;
        }
        replaceCards(cardsRef.current.filter((card) => card.id !== cardId));
        replaceStagedCards(
          stagedCardsRef.current.filter((card) => card.id !== cardId),
        );
        setPreviewCardId((current) => (current === cardId ? null : current));
        if (usesPartitionedBoard) {
          const nextTabs = await refreshTabs();
          if (isMountedRef.current) {
            setTabs(nextTabs);
          }
        }
      } catch (reason) {
        if (isMountedRef.current) {
          setError(getErrorMessage(reason));
        }
      }
    })();
  }, [
    notifySiblingBoards,
    refreshTabs,
    replaceCards,
    replaceStagedCards,
    usesPartitionedBoard,
  ]);

  const persistCardUpdate = useCallback(
    async (
      card: ResponseBoardCard,
      update: Parameters<typeof updateResponseBoardCard>[1],
    ) => {
      const persisted = await updateResponseBoardCard(card.id, update);
      notifySiblingBoards();
      if (!isMountedRef.current) {
        return persisted;
      }
      const withoutPersistedCard = cardsRef.current.filter(
        (candidate) => candidate.id !== persisted.id,
      );
      const withoutPersistedStagedCard = stagedCardsRef.current.filter(
        (candidate) => candidate.id !== persisted.id,
      );
      if (persisted.placement === "staged") {
        replaceCards(withoutPersistedCard);
        replaceStagedCards([...withoutPersistedStagedCard, persisted]);
      } else {
        replaceStagedCards(withoutPersistedStagedCard);
        replaceCards(
          persisted.tabId === selectedTabIdRef.current
            ? [...withoutPersistedCard, persisted]
            : withoutPersistedCard,
        );
      }
      const nextTabs = await refreshTabs();
      if (isMountedRef.current) {
        setTabs(nextTabs);
      }
      return persisted;
    },
    [
      notifySiblingBoards,
      refreshTabs,
      replaceCards,
      replaceStagedCards,
      selectedTabIdRef,
    ],
  );

  const handlePlaceStagedCard = useCallback(
    (card: ResponseBoardCard, position?: { x: number; y: number }) => {
      const nextPosition =
        position ??
        (card.hasCanvasPosition
          ? { x: card.x, y: card.y }
          : nextResponseBoardCardPosition(
              cardsRef.current.filter(
                (candidate) => candidate.placement === "placed",
              ),
            ));
      setError(null);
      void persistCardUpdate(card, {
        ...(selectedTabId ? { tabId: selectedTabId } : {}),
        placement: "placed",
        x: nextPosition.x,
        y: nextPosition.y,
      }).catch((reason) => {
        if (isMountedRef.current) {
          setError(getErrorMessage(reason));
        }
      });
    },
    [persistCardUpdate, selectedTabId],
  );

  const handleReturnToStaging = useCallback(
    (card: ResponseBoardCard) => {
      setError(null);
      void persistCardUpdate(card, { placement: "staged" }).catch((reason) => {
        if (isMountedRef.current) {
          setError(getErrorMessage(reason));
        }
      });
    },
    [persistCardUpdate],
  );

  const handleMoveCard = useCallback(
    (card: ResponseBoardCard, tabId: string) => {
      if (!tabId || tabId === card.tabId) {
        return;
      }
      setError(null);
      void persistCardUpdate(card, { tabId }).then(
        () => {
          if (isMountedRef.current) {
            setPreviewCardId(null);
          }
        },
        (reason) => {
          if (isMountedRef.current) {
            setError(getErrorMessage(reason));
          }
        },
      );
    },
    [persistCardUpdate],
  );

  const handleCreateTab = useCallback(() => {
    const name = newTabName.trim();
    if (!name) {
      return;
    }
    setError(null);
    void (async () => {
      try {
        const tab = await createResponseBoardTab(name);
        notifySiblingBoards();
        const nextTabs = await refreshTabs();
        if (!isMountedRef.current) {
          return;
        }
        setTabs(nextTabs);
        setSelectedTabId(tab.id);
        setNewTabName("");
        setIsAddingTab(false);
      } catch (reason) {
        if (isMountedRef.current) {
          setError(getErrorMessage(reason));
        }
      }
    })();
  }, [newTabName, notifySiblingBoards, refreshTabs]);

  const handleRenameTab = useCallback(
    (tabId: string) => {
      const name = renameValue.trim();
      if (!name) {
        return;
      }
      setError(null);
      void (async () => {
        try {
          await updateResponseBoardTab(tabId, name);
          notifySiblingBoards();
          const nextTabs = await refreshTabs();
          if (!isMountedRef.current) {
            return;
          }
          setTabs(nextTabs);
          setRenamingTabId(null);
        } catch (reason) {
          if (isMountedRef.current) {
            setError(getErrorMessage(reason));
          }
        }
      })();
    },
    [notifySiblingBoards, refreshTabs, renameValue],
  );

  const handleDeleteTab = useCallback(
    (tab: ResponseBoardTab) => {
      setError(null);
      void (async () => {
        try {
          await deleteResponseBoardTab(tab.id);
          notifySiblingBoards();
          const nextTabs = await refreshTabs();
          if (!isMountedRef.current) {
            return;
          }
          setTabs(nextTabs);
          setSelectedTabId((current) =>
            current === tab.id ? (nextTabs[0]?.id ?? null) : current,
          );
        } catch (reason) {
          if (isMountedRef.current) {
            setError(getErrorMessage(reason));
          }
        }
      })();
    },
    [notifySiblingBoards, refreshTabs],
  );

  const handleReorderTab = useCallback(
    (tabId: string, offset: -1 | 1) => {
      if (tabReorderPendingRef.current) {
        return;
      }
      const index = tabs.findIndex((tab) => tab.id === tabId);
      const targetIndex = index + offset;
      if (index < 0 || targetIndex < 0 || targetIndex >= tabs.length) {
        return;
      }
      const nextTabs = [...tabs];
      [nextTabs[index], nextTabs[targetIndex]] = [
        nextTabs[targetIndex],
        nextTabs[index],
      ];
      setTabs(nextTabs);
      setError(null);
      tabReorderPendingRef.current = true;
      setIsReorderingTabs(true);
      void (async () => {
        try {
          const response = await reorderResponseBoardTabs(
            nextTabs.map((tab) => tab.id),
          );
          notifySiblingBoards();
          if (isMountedRef.current) {
            setTabs(response.tabs);
          }
        } catch (reason) {
          if (isMountedRef.current) {
            setTabs(tabs);
            setError(getErrorMessage(reason));
          }
        } finally {
          tabReorderPendingRef.current = false;
          if (isMountedRef.current) {
            setIsReorderingTabs(false);
          }
        }
      })();
    },
    [notifySiblingBoards, tabs],
  );

  const handleDrop = useCallback((event: React.DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    const surface = surfaceRef.current;
    if (!surface) {
      return;
    }
    const rect = surface.getBoundingClientRect();
    const clientX = Number.isFinite(event.clientX)
      ? event.clientX
      : rect.left + surface.clientWidth / 2;
    const clientY = Number.isFinite(event.clientY)
      ? event.clientY
      : rect.top + surface.clientHeight / 2;
    const x = Math.max(
      0,
      (clientX - rect.left - view.panX) / view.zoom - 180,
    );
    const y = Math.max(
      0,
      (clientY - rect.top - view.panY) / view.zoom - 28,
    );
    setError(null);
    const stagedCardId = event.dataTransfer.getData(
      RESPONSE_BOARD_STAGED_CARD_MIME,
    );
    if (usesPartitionedBoard && stagedCardId) {
      const card = stagedCardsRef.current.find(
        (candidate) => candidate.id === stagedCardId,
      );
      if (card) {
        handlePlaceStagedCard(card, { x, y });
      }
      return;
    }
    const source = readResponseBoardMessageDragData(event.dataTransfer);
    if (!source) {
      return;
    }
    if (!usesPartitionedBoard || !selectedTabId) {
      void createResponseBoardCard({ ...source, x, y }).then(
        (card) => {
          notifySiblingBoards();
          if (isMountedRef.current) {
            replaceCards([...cardsRef.current, card]);
          }
        },
        (reason) => {
          if (isMountedRef.current) {
            setError(getErrorMessage(reason));
          }
        },
      );
      return;
    }
    const destinationTabId = selectedTabId;
    void stageResponseBoardCard({
      ...source,
      tabId: destinationTabId,
      placement: "placed",
      x,
      y,
    })
      .then((card) => {
        notifySiblingBoards();
        if (
          isMountedRef.current &&
          selectedTabIdRef.current === destinationTabId
        ) {
          replaceStagedCards(
            stagedCardsRef.current.filter(
              (candidate) => candidate.id !== card.id,
            ),
          );
          replaceCards([
            ...cardsRef.current.filter(
              (candidate) => candidate.id !== card.id,
            ),
            card,
          ]);
        }
      })
      .catch((reason) => {
        if (isMountedRef.current) {
          setError(getErrorMessage(reason));
        }
      });
  }, [
    handlePlaceStagedCard,
    notifySiblingBoards,
    replaceCards,
    replaceStagedCards,
    selectedTabId,
    usesPartitionedBoard,
    view.panX,
    view.panY,
    view.zoom,
  ]);

  const zoomAtSurfaceCenter = useCallback(
    (resolveZoom: (currentZoom: number) => number) => {
      const surface = surfaceRef.current;
      if (!surface) {
        return;
      }
      const rect = surface.getBoundingClientRect();
      setView((current) =>
        zoomBoardViewAtPoint(
          current,
          resolveZoom(current.zoom),
          rect.width / 2,
          rect.height / 2,
        ),
      );
    },
    [],
  );

  const fitBoardInView = useCallback(() => {
    const surface = surfaceRef.current;
    if (!surface) {
      return;
    }
    const bounds = surface.getBoundingClientRect();
    const fittedView = fitResponseBoardCardsInView(
      cardsRef.current,
      bounds.width,
      bounds.height,
    );
    setView(fittedView ?? { panX: 0, panY: 0, zoom: 1 });
  }, []);

  const handleBoardWheel = useCallback((event: WheelEvent) => {
    const surface = surfaceRef.current;
    if (!wheelRequestsBoardZoom(event) || !surface) {
      return;
    }
    event.preventDefault();
    const rect = surface.getBoundingClientRect();
    const surfaceX = event.clientX - rect.left;
    const surfaceY = event.clientY - rect.top;
    setView((current) =>
      zoomBoardViewAtPoint(
        current,
        current.zoom * Math.exp(-event.deltaY * BOARD_WHEEL_ZOOM_SENSITIVITY),
        surfaceX,
        surfaceY,
      ),
    );
  }, []);

  useEffect(() => {
    const surface = surfaceRef.current;
    if (!surface) {
      return;
    }
    surface.addEventListener("wheel", handleBoardWheel, { passive: false });
    return () => surface.removeEventListener("wheel", handleBoardWheel);
  }, [handleBoardWheel]);

  const handleBoardKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (!(event.ctrlKey || event.metaKey)) {
        return;
      }
      if (event.key === "+" || event.key === "=") {
        event.preventDefault();
        zoomAtSurfaceCenter((zoom) => zoom * BOARD_ZOOM_BUTTON_FACTOR);
      } else if (event.key === "-" || event.key === "_") {
        event.preventDefault();
        zoomAtSurfaceCenter((zoom) => zoom / BOARD_ZOOM_BUTTON_FACTOR);
      } else if (event.key === "0") {
        event.preventDefault();
        fitBoardInView();
      }
    },
    [fitBoardInView, zoomAtSurfaceCenter],
  );

  const handlePanPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const target = event.target as Element;
      if (event.button !== 0 || target.closest(".response-board-card")) {
        return;
      }
      event.preventDefault();
      window.getSelection()?.removeAllRanges();
      event.currentTarget.focus({ preventScroll: true });
      event.currentTarget.setPointerCapture?.(event.pointerId);
      setIsPanning(true);
      panGestureRef.current = {
        pointerId: event.pointerId,
        clientX: event.clientX,
        clientY: event.clientY,
        panX: view.panX,
        panY: view.panY,
      };
    },
    [view.panX, view.panY],
  );

  const handlePanPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const gesture = panGestureRef.current;
      if (!gesture || gesture.pointerId !== event.pointerId) {
        return;
      }
      setView((current) => ({
        ...current,
        panX: gesture.panX + (event.clientX - gesture.clientX),
        panY: gesture.panY + (event.clientY - gesture.clientY),
      }));
    },
    [],
  );

  const finishPan = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    if (panGestureRef.current?.pointerId !== event.pointerId) {
      return;
    }
    panGestureRef.current = null;
    setIsPanning(false);
    event.currentTarget.releasePointerCapture?.(event.pointerId);
  }, []);

  const placedCards = useMemo(
    () =>
      usesPartitionedBoard
        ? cards.filter((card) => card.placement === "placed")
        : cards,
    [cards, usesPartitionedBoard],
  );
  const stagedCardsNewestFirst = useMemo(
    () => [...stagedCards].reverse(),
    [stagedCards],
  );
  const previewCard = useMemo(
    () => stagedCards.find((card) => card.id === previewCardId) ?? null,
    [previewCardId, stagedCards],
  );
  const selectedTab = tabs.find((tab) => tab.id === selectedTabId) ?? null;

  const planeSize = useMemo(
    () => ({
      width: Math.max(
        0,
        ...placedCards.map((card) => card.x + card.w + BOARD_PADDING),
      ),
      height: Math.max(
        0,
        ...placedCards.map((card) => card.y + card.h + BOARD_PADDING),
      ),
    }),
    [placedCards],
  );

  return (
    <section className="response-board-panel" aria-label="Response board">
      <header className="response-board-toolbar">
        <div className="response-board-toolbar-copy">
          <strong>Response board</strong>
          <span>Drag any transcript message here or use Pin to board.</span>
        </div>
        <div className="response-board-toolbar-actions">
          <div className="response-board-zoom-controls" aria-label="Board zoom controls">
            <button
              type="button"
              aria-label="Zoom out"
              onClick={() =>
                zoomAtSurfaceCenter((zoom) => zoom / BOARD_ZOOM_BUTTON_FACTOR)
              }
            >
              −
            </button>
            <button
              type="button"
              className="response-board-zoom-reset"
              aria-label="Fit board in view"
              onClick={fitBoardInView}
            >
              {Math.round(view.zoom * 100)}%
            </button>
            <button
              type="button"
              aria-label="Zoom in"
              onClick={() =>
                zoomAtSurfaceCenter((zoom) => zoom * BOARD_ZOOM_BUTTON_FACTOR)
              }
            >
              +
            </button>
          </div>
          <span className="response-board-card-count">
            {usesPartitionedBoard
              ? `${placedCards.length} placed`
              : `${cards.length} cards`}
          </span>
        </div>
      </header>
      {usesPartitionedBoard ? (
        <ResponseBoardTabStrip
          tabs={tabs}
          workspaceTabId={workspaceTabId}
          selectedTabId={selectedTabId}
          renamingTabId={renamingTabId}
          renameValue={renameValue}
          isAddingTab={isAddingTab}
          newTabName={newTabName}
          isReorderingTabs={isReorderingTabs}
          tabButtonRefs={tabButtonRefs}
          onSelectTab={setSelectedTabId}
          onRenameValueChange={setRenameValue}
          onCancelRename={() => setRenamingTabId(null)}
          onSubmitRename={handleRenameTab}
          onStartRename={(tab) => {
            setRenameValue(tab.name);
            setRenamingTabId(tab.id);
          }}
          onReorderTab={handleReorderTab}
          onDeleteTab={handleDeleteTab}
          onNewTabNameChange={setNewTabName}
          onCancelAdd={() => {
            setIsAddingTab(false);
            setNewTabName("");
          }}
          onSubmitAdd={handleCreateTab}
          onStartAdd={() => setIsAddingTab(true)}
        />
      ) : null}
      {error ? (
        <div className="response-board-error" role="alert">
          {error}
          <button type="button" onClick={() => setError(null)} aria-label="Dismiss error">
            ×
          </button>
        </div>
      ) : null}
      {usesPartitionedBoard && selectedTab ? (
        <ResponseBoardStagingTray
          cards={stagedCardsNewestFirst}
          previewCardId={previewCardId}
          onPreview={setPreviewCardId}
        />
      ) : null}
      <div
        ref={surfaceRef}
        id={
          usesPartitionedBoard && selectedTabId
            ? `response-board-tabpanel-${workspaceTabId}-${selectedTabId}`
            : undefined
        }
        role={usesPartitionedBoard ? "tabpanel" : undefined}
        aria-labelledby={
          usesPartitionedBoard && selectedTabId
            ? `response-board-tab-${workspaceTabId}-${selectedTabId}`
            : undefined
        }
        className={`response-board-surface${isPanning ? " is-panning" : ""}`}
        tabIndex={0}
        aria-label="Response board canvas"
        aria-keyshortcuts="Control+= Meta+= Control+- Meta+- Control+0 Meta+0"
        onDragOver={(event) => {
          event.preventDefault();
          event.dataTransfer.dropEffect = Array.from(
            event.dataTransfer.types,
          ).includes(RESPONSE_BOARD_STAGED_CARD_MIME)
            ? "move"
            : "copy";
        }}
        onDrop={handleDrop}
        onPointerDown={handlePanPointerDown}
        onPointerMove={handlePanPointerMove}
        onPointerUp={finishPan}
        onPointerCancel={finishPan}
        onLostPointerCapture={finishPan}
        onKeyDown={handleBoardKeyDown}
      >
        {usesPartitionedBoard && previewCard ? (
          <ResponseBoardPreview
            card={previewCard}
            selectedTabName={selectedTab?.name ?? null}
            onPlace={() => handlePlaceStagedCard(previewCard)}
            onOpenSource={() => onOpenSource(previewCard)}
            onDelete={() => handleRemove(previewCard.id)}
            onClose={() => setPreviewCardId(null)}
          />
        ) : null}
        <div
          className="response-board-plane"
          style={{
            width: planeSize.width,
            height: planeSize.height,
            minWidth: `${100 / view.zoom}%`,
            minHeight: `${100 / view.zoom}%`,
            transform: `translate(${view.panX}px, ${view.panY}px) scale(${view.zoom})`,
          }}
        >
          {isLoading ? (
            <div className="response-board-empty">Loading response board…</div>
          ) : placedCards.length === 0 ? (
            <div className="response-board-empty">
              {usesPartitionedBoard
                ? "Drag a staged response here, or drop a transcript message."
                : "Drop an agent response anywhere on the board."}
            </div>
          ) : null}
          {placedCards.map((card) => (
            <article
              key={card.id}
              className="response-board-card"
              style={{
                left: card.x,
                top: card.y,
                width: card.w,
                height: card.h,
              }}
              onPointerMove={handleCardPointerMove}
              onPointerUp={finishCardGesture}
              onPointerCancel={finishCardGesture}
            >
              <header
                className="response-board-card-header"
                onPointerDown={(event) => beginCardGesture(event, card, "move")}
              >
                <span
                  className={`response-board-agent-dot is-${card.sourceAgent.toLowerCase()}`}
                  aria-hidden="true"
                />
                <span className="response-board-card-title">
                  {card.sourceSessionName}
                </span>
                <time>{card.snapshot.timestamp}</time>
                <button
                  type="button"
                  className="response-board-remove"
                  aria-label={`Remove response from ${card.sourceSessionName}`}
                  onPointerDown={(event) => event.stopPropagation()}
                  onClick={() => handleRemove(card.id)}
                >
                  ×
                </button>
              </header>
              <div className="response-board-card-body">
                <div className="response-board-card-snapshot">
                  <MessageCard
                    message={card.snapshot}
                    approvalActionsEnabled={false}
                    parallelAgentActionsEnabled={false}
                    preferImmediateHeavyRender
                    onApprovalDecision={() => {}}
                    onUserInputSubmit={() => {}}
                  />
                </div>
              </div>
              <footer className="response-board-card-footer">
                {usesPartitionedBoard ? (
                  <>
                    <button
                      type="button"
                      onClick={() => handleReturnToStaging(card)}
                    >
                      Return to staging
                    </button>
                    <label>
                      Move
                      <select
                        value={card.tabId}
                        onChange={(event) =>
                          handleMoveCard(card, event.target.value)
                        }
                      >
                        {tabs.map((tab) => (
                          <option key={tab.id} value={tab.id}>
                            {tab.name}
                          </option>
                        ))}
                      </select>
                    </label>
                  </>
                ) : null}
                <button type="button" onClick={() => onOpenSource(card)}>
                  Open in session
                </button>
              </footer>
              <button
                type="button"
                className="response-board-resize-handle"
                aria-label={`Resize response from ${card.sourceSessionName}`}
                onPointerDown={(event) => beginCardGesture(event, card, "resize")}
              />
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}
