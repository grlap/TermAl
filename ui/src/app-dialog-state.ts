import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import { LOCAL_REMOTE_ID } from "./remotes";
import { clamp, type SessionFlagMap } from "./app-utils";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import {
  CREATE_SESSION_WORKSPACE_ID,
  PENDING_KILL_CLOSE_DELAY_MS,
  PENDING_SESSION_RENAME_CLOSE_DELAY_MS,
  type PendingSessionRename,
} from "./app-shell-internals";
import type { PreferencesTabId } from "./preferences/preferences-tabs";
import type { Project, Session } from "./types";

type UseAppDialogStateArgs = {
  selectedProjectId: string;
  activeSession: Session | null;
  workspaceActivePaneId: string | null;
  projectLookup: Map<string, Project>;
  sessionLookup: Map<string, Session>;
  updatingSessionIds: SessionFlagMap;
  killingSessionIds: SessionFlagMap;
  isCreating: boolean;
  killRevealSessionId: string | null;
  setKillRevealSessionId: Dispatch<SetStateAction<string | null>>;
  pendingKillSessionId: string | null;
  setPendingKillSessionId: Dispatch<SetStateAction<string | null>>;
  pendingSessionRename: PendingSessionRename | null;
  setPendingSessionRename: Dispatch<SetStateAction<PendingSessionRename | null>>;
  setIsCreateSessionOpen: Dispatch<SetStateAction<boolean>>;
  setCreateSessionPaneId: Dispatch<SetStateAction<string | null>>;
  setCreateSessionProjectId: Dispatch<SetStateAction<string>>;
  setIsCreateProjectOpen: Dispatch<SetStateAction<boolean>>;
  setNewProjectRemoteId: Dispatch<SetStateAction<string>>;
  clearRequestError: () => void;
  executeKillSession: (sessionId: string) => Promise<void>;
  handleRenameSession: (sessionId: string, nextName: string) => Promise<boolean>;
  handleCloneSessionFromExisting: (sessionId: string) => Promise<boolean>;
};

type UseAppDialogStateResult = {
  isSettingsOpen: boolean;
  setIsSettingsOpen: Dispatch<SetStateAction<boolean>>;
  settingsTab: PreferencesTabId;
  setSettingsTab: Dispatch<SetStateAction<PreferencesTabId>>;
  killRevealSessionId: string | null;
  setKillRevealSessionId: Dispatch<SetStateAction<string | null>>;
  pendingKillSessionId: string | null;
  pendingKillSession: Session | null;
  pendingKillPopoverStyle: CSSProperties | null;
  pendingKillPopoverRef: RefObject<HTMLDivElement | null>;
  pendingKillConfirmButtonRef: RefObject<HTMLButtonElement | null>;
  handleKillSession: (sessionId: string, trigger?: HTMLButtonElement | null) => void;
  confirmKillSession: () => Promise<void>;
  clearPendingKillCloseTimeout: () => void;
  schedulePendingKillConfirmationClose: () => void;
  closePendingKillConfirmation: (restoreFocus?: boolean) => void;
  pendingSessionRenameSession: Session | null;
  pendingSessionRenameDraft: string;
  setPendingSessionRenameDraft: Dispatch<SetStateAction<string>>;
  pendingSessionRenameValue: string;
  isPendingSessionRenameSubmitting: boolean;
  isPendingSessionRenameCreating: boolean;
  isPendingSessionRenameKilling: boolean;
  pendingSessionRenameStyle: CSSProperties | null;
  pendingSessionRenamePopoverRef: RefObject<HTMLFormElement | null>;
  pendingSessionRenameInputRef: RefObject<HTMLInputElement | null>;
  handleSessionRenameRequest: (
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) => void;
  confirmSessionRename: () => Promise<void>;
  handlePendingSessionRenameNew: () => Promise<void>;
  handlePendingSessionRenameKill: () => Promise<void>;
  clearPendingSessionRenameCloseTimeout: () => void;
  schedulePendingSessionRenameClose: () => void;
  closePendingSessionRename: (restoreFocus?: boolean) => void;
  openCreateSessionDialog: (
    preferredPaneId?: string | null,
    defaultProjectSelectionId?: string | null,
  ) => void;
  openCreateProjectDialog: () => void;
};

export function useAppDialogState({
  selectedProjectId,
  activeSession,
  workspaceActivePaneId,
  projectLookup,
  sessionLookup,
  updatingSessionIds,
  killingSessionIds,
  isCreating,
  killRevealSessionId,
  setKillRevealSessionId,
  pendingKillSessionId,
  setPendingKillSessionId,
  pendingSessionRename,
  setPendingSessionRename,
  setIsCreateSessionOpen,
  setCreateSessionPaneId,
  setCreateSessionProjectId,
  setIsCreateProjectOpen,
  setNewProjectRemoteId,
  clearRequestError,
  executeKillSession,
  handleRenameSession,
  handleCloneSessionFromExisting,
}: UseAppDialogStateArgs): UseAppDialogStateResult {
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<PreferencesTabId>("themes");
  const [pendingSessionRenameDraft, setPendingSessionRenameDraft] =
    useState("");
  const [pendingSessionRenameStyle, setPendingSessionRenameStyle] =
    useState<CSSProperties | null>(null);
  const [pendingKillPopoverStyle, setPendingKillPopoverStyle] =
    useState<CSSProperties | null>(null);
  const pendingSessionRenameTriggerRef = useRef<HTMLElement | null>(null);
  const pendingSessionRenamePopoverRef = useRef<HTMLFormElement | null>(null);
  const pendingSessionRenameInputRef = useRef<HTMLInputElement | null>(null);
  const pendingSessionRenameCloseTimeoutRef = useRef<number | null>(null);
  const pendingKillTriggerRef = useRef<HTMLButtonElement | null>(null);
  const pendingKillPopoverRef = useRef<HTMLDivElement | null>(null);
  const pendingKillConfirmButtonRef = useRef<HTMLButtonElement | null>(null);
  const pendingKillCloseTimeoutRef = useRef<number | null>(null);

  const pendingSessionRenameSession = useMemo(
    () =>
      pendingSessionRename
        ? (sessionLookup.get(pendingSessionRename.sessionId) ?? null)
        : null,
    [pendingSessionRename, sessionLookup],
  );
  const pendingSessionRenameValue = pendingSessionRenameDraft.trim();
  const isPendingSessionRenameSubmitting = pendingSessionRenameSession
    ? Boolean(updatingSessionIds[pendingSessionRenameSession.id])
    : false;
  const isPendingSessionRenameCreating = pendingSessionRenameSession
    ? isCreating
    : false;
  const isPendingSessionRenameKilling = pendingSessionRenameSession
    ? Boolean(killingSessionIds[pendingSessionRenameSession.id])
    : false;
  const pendingKillSession = pendingKillSessionId
    ? (sessionLookup.get(pendingKillSessionId) ?? null)
    : null;

  const clearPendingKillCloseTimeout = useCallback(() => {
    if (pendingKillCloseTimeoutRef.current === null) {
      return;
    }

    window.clearTimeout(pendingKillCloseTimeoutRef.current);
    pendingKillCloseTimeoutRef.current = null;
  }, []);

  const focusPendingKillTrigger = useCallback(() => {
    window.requestAnimationFrame(() => {
      pendingKillTriggerRef.current?.focus();
    });
  }, []);

  const closePendingKillConfirmation = useCallback(
    (restoreFocus = false) => {
      clearPendingKillCloseTimeout();
      setPendingKillSessionId(null);
      setPendingKillPopoverStyle(null);
      if (restoreFocus) {
        focusPendingKillTrigger();
      }
    },
    [clearPendingKillCloseTimeout, focusPendingKillTrigger],
  );

  const schedulePendingKillConfirmationClose = useCallback(() => {
    clearPendingKillCloseTimeout();

    const sessionId = pendingKillSessionId;
    if (!sessionId) {
      return;
    }

    pendingKillCloseTimeoutRef.current = window.setTimeout(() => {
      pendingKillCloseTimeoutRef.current = null;
      setPendingKillSessionId((current) =>
        current === sessionId ? null : current,
      );
      setPendingKillPopoverStyle(null);
    }, PENDING_KILL_CLOSE_DELAY_MS);
  }, [clearPendingKillCloseTimeout, pendingKillSessionId]);

  const clearPendingSessionRenameCloseTimeout = useCallback(() => {
    if (pendingSessionRenameCloseTimeoutRef.current === null) {
      return;
    }

    window.clearTimeout(pendingSessionRenameCloseTimeoutRef.current);
    pendingSessionRenameCloseTimeoutRef.current = null;
  }, []);

  const focusPendingSessionRenameTrigger = useCallback(() => {
    window.requestAnimationFrame(() => {
      pendingSessionRenameTriggerRef.current?.focus();
    });
  }, []);

  const closePendingSessionRename = useCallback(
    (restoreFocus = false) => {
      clearPendingSessionRenameCloseTimeout();
      setPendingSessionRename(null);
      setPendingSessionRenameDraft("");
      setPendingSessionRenameStyle(null);
      if (restoreFocus) {
        focusPendingSessionRenameTrigger();
      }
    },
    [clearPendingSessionRenameCloseTimeout, focusPendingSessionRenameTrigger],
  );

  const schedulePendingSessionRenameClose = useCallback(() => {
    clearPendingSessionRenameCloseTimeout();

    const pendingRename = pendingSessionRename;
    if (!pendingRename) {
      return;
    }
    if (pendingSessionRenameInputRef.current === document.activeElement) {
      return;
    }

    pendingSessionRenameCloseTimeoutRef.current = window.setTimeout(() => {
      pendingSessionRenameCloseTimeoutRef.current = null;
      setPendingSessionRename((current) =>
        current?.sessionId === pendingRename.sessionId ? null : current,
      );
      setPendingSessionRenameDraft("");
      setPendingSessionRenameStyle(null);
    }, PENDING_SESSION_RENAME_CLOSE_DELAY_MS);
  }, [clearPendingSessionRenameCloseTimeout, pendingSessionRename]);

  const openCreateSessionDialog = useCallback(
    (
      preferredPaneId: string | null = null,
      defaultProjectSelectionId: string | null = null,
    ) => {
      const normalizedDefaultProjectSelectionId =
        defaultProjectSelectionId?.trim() ?? "";
      const fallbackProjectId =
        selectedProjectId !== ALL_PROJECTS_FILTER_ID &&
        projectLookup.has(selectedProjectId)
          ? selectedProjectId
          : activeSession?.projectId && projectLookup.has(activeSession.projectId)
            ? activeSession.projectId
            : CREATE_SESSION_WORKSPACE_ID;
      const defaultProjectId =
        normalizedDefaultProjectSelectionId === CREATE_SESSION_WORKSPACE_ID
          ? CREATE_SESSION_WORKSPACE_ID
          : normalizedDefaultProjectSelectionId &&
              projectLookup.has(normalizedDefaultProjectSelectionId)
            ? normalizedDefaultProjectSelectionId
            : fallbackProjectId;

      setCreateSessionPaneId(preferredPaneId ?? workspaceActivePaneId);
      setCreateSessionProjectId(defaultProjectId);
      clearRequestError();
      setIsCreateSessionOpen(true);
    },
    [
      activeSession,
      clearRequestError,
      projectLookup,
      selectedProjectId,
      setCreateSessionPaneId,
      setCreateSessionProjectId,
      setIsCreateSessionOpen,
      workspaceActivePaneId,
    ],
  );

  const openCreateProjectDialog = useCallback(() => {
    setNewProjectRemoteId(LOCAL_REMOTE_ID);
    clearRequestError();
    setIsCreateProjectOpen(true);
  }, [clearRequestError, setIsCreateProjectOpen, setNewProjectRemoteId]);

  const handleKillSession = useCallback(
    (sessionId: string, trigger?: HTMLButtonElement | null) => {
      const session = sessionLookup.get(sessionId);
      if (!session) {
        return;
      }

      closePendingSessionRename();
      clearPendingKillCloseTimeout();
      pendingKillTriggerRef.current = trigger ?? null;
      setPendingKillSessionId((current) =>
        current === sessionId ? null : sessionId,
      );
    },
    [
      clearPendingKillCloseTimeout,
      closePendingSessionRename,
      sessionLookup,
    ],
  );

  const confirmKillSession = useCallback(async () => {
    if (!pendingKillSessionId) {
      return;
    }

    const sessionId = pendingKillSessionId;
    setPendingKillSessionId(null);
    setKillRevealSessionId(null);

    await executeKillSession(sessionId);
  }, [executeKillSession, pendingKillSessionId]);

  const handleSessionRenameRequest = useCallback(
    (
      sessionId: string,
      clientX: number,
      clientY: number,
      trigger?: HTMLElement | null,
    ) => {
      const session = sessionLookup.get(sessionId);
      if (!session) {
        return;
      }

      closePendingKillConfirmation();
      clearPendingSessionRenameCloseTimeout();
      pendingSessionRenameTriggerRef.current = trigger ?? null;
      setPendingSessionRenameDraft(session.name);
      setPendingSessionRename({
        sessionId,
        clientX,
        clientY,
      });
    },
    [
      clearPendingSessionRenameCloseTimeout,
      closePendingKillConfirmation,
      sessionLookup,
    ],
  );

  const confirmSessionRename = useCallback(async () => {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    const nextName = pendingSessionRenameDraft.trim();
    if (!session) {
      closePendingSessionRename();
      return;
    }
    if (!nextName) {
      return;
    }
    if (nextName === session.name.trim()) {
      closePendingSessionRename(true);
      return;
    }

    const renamed = await handleRenameSession(session.id, nextName);
    if (renamed) {
      closePendingSessionRename();
    }
  }, [
    closePendingSessionRename,
    handleRenameSession,
    pendingSessionRename,
    pendingSessionRenameDraft,
    sessionLookup,
  ]);

  const handlePendingSessionRenameNew = useCallback(async () => {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    if (!session) {
      closePendingSessionRename();
      return;
    }

    const created = await handleCloneSessionFromExisting(session.id);
    if (created) {
      closePendingSessionRename();
    }
  }, [
    closePendingSessionRename,
    handleCloneSessionFromExisting,
    pendingSessionRename,
    sessionLookup,
  ]);

  const handlePendingSessionRenameKill = useCallback(async () => {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    if (!session) {
      closePendingSessionRename();
      return;
    }

    closePendingSessionRename();
    setKillRevealSessionId(null);
    await executeKillSession(session.id);
  }, [
    closePendingSessionRename,
    executeKillSession,
    pendingSessionRename,
    sessionLookup,
  ]);

  useEffect(() => {
    return () => {
      clearPendingKillCloseTimeout();
      clearPendingSessionRenameCloseTimeout();
    };
  }, [clearPendingKillCloseTimeout, clearPendingSessionRenameCloseTimeout]);

  useEffect(() => {
    if (!pendingKillSessionId) {
      clearPendingKillCloseTimeout();
      return;
    }

    const focusFrameId = window.requestAnimationFrame(() => {
      pendingKillConfirmButtonRef.current?.focus();
    });

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        closePendingKillConfirmation(true);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrameId);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [
    clearPendingKillCloseTimeout,
    closePendingKillConfirmation,
    pendingKillSessionId,
  ]);

  useLayoutEffect(() => {
    if (!pendingKillSessionId) {
      setPendingKillPopoverStyle(null);
      return;
    }

    setPendingKillPopoverStyle({
      left: 0,
      top: 0,
      visibility: "hidden",
    });

    function updatePendingKillPopoverStyle() {
      const trigger = pendingKillTriggerRef.current;
      const popover = pendingKillPopoverRef.current;
      if (!trigger || !popover) {
        return;
      }

      const triggerRect = trigger.getBoundingClientRect();
      const popoverRect = popover.getBoundingClientRect();
      const viewportPadding = 12;
      const preferredLeft =
        triggerRect.left + triggerRect.width / 2 - popoverRect.width / 2;
      const left = clamp(
        preferredLeft,
        viewportPadding,
        window.innerWidth - popoverRect.width - viewportPadding,
      );
      const preferredTop = triggerRect.top - 10;
      const top = clamp(
        preferredTop,
        viewportPadding,
        window.innerHeight - popoverRect.height - viewportPadding,
      );

      setPendingKillPopoverStyle({
        left,
        top,
      });
    }

    const frameId = window.requestAnimationFrame(updatePendingKillPopoverStyle);
    window.addEventListener("resize", updatePendingKillPopoverStyle);
    window.addEventListener("scroll", updatePendingKillPopoverStyle, true);

    return () => {
      window.cancelAnimationFrame(frameId);
      window.removeEventListener("resize", updatePendingKillPopoverStyle);
      window.removeEventListener("scroll", updatePendingKillPopoverStyle, true);
    };
  }, [pendingKillSessionId]);

  useEffect(() => {
    if (!pendingSessionRename) {
      clearPendingSessionRenameCloseTimeout();
      return;
    }

    const focusFrameId = window.requestAnimationFrame(() => {
      pendingSessionRenameInputRef.current?.focus();
      pendingSessionRenameInputRef.current?.select();
    });

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        closePendingSessionRename(true);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrameId);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [
    clearPendingSessionRenameCloseTimeout,
    closePendingSessionRename,
    pendingSessionRename,
  ]);

  useLayoutEffect(() => {
    if (!pendingSessionRename) {
      setPendingSessionRenameStyle(null);
      return;
    }

    const renameAnchor = pendingSessionRename;

    setPendingSessionRenameStyle({
      left: 0,
      top: 0,
      visibility: "hidden",
    });

    function updatePendingSessionRenameStyle() {
      const popover = pendingSessionRenamePopoverRef.current;
      if (!popover) {
        return;
      }

      const popoverRect = popover.getBoundingClientRect();
      const viewportPadding = 12;
      const left = clamp(
        renameAnchor.clientX - popoverRect.width / 2,
        viewportPadding,
        window.innerWidth - popoverRect.width - viewportPadding,
      );
      const top = clamp(
        renameAnchor.clientY - 18,
        viewportPadding,
        window.innerHeight - popoverRect.height - viewportPadding,
      );

      setPendingSessionRenameStyle({
        left,
        top,
      });
    }

    const frameId = window.requestAnimationFrame(
      updatePendingSessionRenameStyle,
    );
    window.addEventListener("resize", updatePendingSessionRenameStyle);

    return () => {
      window.cancelAnimationFrame(frameId);
      window.removeEventListener("resize", updatePendingSessionRenameStyle);
    };
  }, [pendingSessionRename]);

  useEffect(() => {
    if (!isSettingsOpen) {
      return;
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setIsSettingsOpen(false);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isSettingsOpen]);

  return {
    isSettingsOpen,
    setIsSettingsOpen,
    settingsTab,
    setSettingsTab,
    killRevealSessionId,
    setKillRevealSessionId,
    pendingKillSessionId,
    pendingKillSession,
    pendingKillPopoverStyle,
    pendingKillPopoverRef,
    pendingKillConfirmButtonRef,
    handleKillSession,
    confirmKillSession,
    clearPendingKillCloseTimeout,
    schedulePendingKillConfirmationClose,
    closePendingKillConfirmation,
    pendingSessionRenameSession,
    pendingSessionRenameDraft,
    setPendingSessionRenameDraft,
    pendingSessionRenameValue,
    isPendingSessionRenameSubmitting,
    isPendingSessionRenameCreating,
    isPendingSessionRenameKilling,
    pendingSessionRenameStyle,
    pendingSessionRenamePopoverRef,
    pendingSessionRenameInputRef,
    handleSessionRenameRequest,
    confirmSessionRename,
    handlePendingSessionRenameNew,
    handlePendingSessionRenameKill,
    clearPendingSessionRenameCloseTimeout,
    schedulePendingSessionRenameClose,
    closePendingSessionRename,
    openCreateSessionDialog,
    openCreateProjectDialog,
  };
}
