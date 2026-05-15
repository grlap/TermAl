import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createRef, type ComponentProps } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { AppDialogs } from "./AppDialogs";
import {
  APPROVAL_POLICY_OPTIONS,
  CLAUDE_APPROVAL_OPTIONS,
  CURSOR_MODE_OPTIONS,
  GEMINI_APPROVAL_OPTIONS,
  SANDBOX_MODE_OPTIONS,
} from "./preferences-panels";
import { type PreferencesTabId } from "./preferences/preferences-tabs";
import {
  DIAGRAM_LOOKS,
  DIAGRAM_PALETTES,
  MARKDOWN_STYLES,
  MARKDOWN_THEMES,
  STYLES,
  THEMES,
} from "./themes";
import type { ClaudeEffortLevel, CodexReasoningEffort } from "./types";

type NavigatorWithUserAgentData = Navigator & {
  userAgentData?: { platform?: string };
};

function createBaseProps(
  overrides: Partial<ComponentProps<typeof AppDialogs>> = {},
): ComponentProps<typeof AppDialogs> {
  const defaultCodexReasoningEffort: CodexReasoningEffort = "medium";
  const defaultClaudeEffort: ClaudeEffortLevel = "default";
  const settingsTab: PreferencesTabId = "appearance";

  return {
    pendingKillSession: null,
    pendingKillPopoverRef: createRef(),
    pendingKillPopoverStyle: null,
    pendingKillConfirmButtonRef: createRef(),
    schedulePendingKillConfirmationClose: vi.fn(),
    clearPendingKillCloseTimeout: vi.fn(),
    closePendingKillConfirmation: vi.fn(),
    confirmKillSession: vi.fn(async () => {}),
    pendingSessionRenameSession: null,
    pendingSessionRenamePopoverRef: createRef(),
    pendingSessionRenameStyle: null,
    pendingSessionRenameInputRef: createRef(),
    pendingSessionRenameDraft: "",
    pendingSessionRenameValue: "",
    isPendingSessionRenameCreating: false,
    isPendingSessionRenameSubmitting: false,
    isPendingSessionRenameKilling: false,
    schedulePendingSessionRenameClose: vi.fn(),
    clearPendingSessionRenameCloseTimeout: vi.fn(),
    closePendingSessionRename: vi.fn(),
    confirmSessionRename: vi.fn(async () => {}),
    setPendingSessionRenameDraft: vi.fn(),
    handlePendingSessionRenameNew: vi.fn(async () => {}),
    handlePendingSessionRenameKill: vi.fn(async () => {}),
    requestError: null,
    isCreateSessionOpen: false,
    isCreating: false,
    closeCreateSessionDialog: vi.fn(),
    handleCreateSessionDialogSubmit: vi.fn(async () => {}),
    newSessionAgent: "Codex",
    onChangeNewSessionAgent: vi.fn(),
    createSessionUsesSessionModelPicker: false,
    newSessionModel: "gpt-5.4",
    newSessionModelOptions: [{ label: "gpt-5.4", value: "gpt-5.4" }],
    onChangeNewSessionModel: vi.fn(),
    defaultCodexModel: "default",
    handleDefaultCodexModelChange: vi.fn(),
    defaultCodexReasoningEffort,
    handleDefaultCodexReasoningEffortChange: vi.fn(),
    defaultClaudeModel: "default",
    handleDefaultClaudeModelChange: vi.fn(),
    defaultClaudeEffort,
    handleDefaultClaudeEffortChange: vi.fn(),
    defaultCursorModel: "default",
    handleDefaultCursorModelChange: vi.fn(),
    defaultCursorMode: CURSOR_MODE_OPTIONS[0].value,
    onChangeDefaultCursorMode: vi.fn(),
    defaultGeminiModel: "default",
    handleDefaultGeminiModelChange: vi.fn(),
    defaultGeminiApprovalMode: GEMINI_APPROVAL_OPTIONS[0].value,
    onChangeDefaultGeminiApprovalMode: vi.fn(),
    createSessionProjectId: "",
    createSessionProjectOptions: [{ label: "Current workspace", value: "" }],
    onChangeCreateSessionProjectId: vi.fn(),
    createSessionProjectHint: "Current workspace",
    createSessionProjectSelectionError: null,
    createSessionAgentReadiness: null,
    createSessionBlocked: false,
    isCreateProjectOpen: false,
    isCreatingProject: false,
    closeCreateProjectDialog: vi.fn(),
    handleCreateProject: vi.fn(async () => true),
    newProjectRemoteId: "local",
    createProjectRemoteOptions: [{ label: "Local", value: "local" }],
    onChangeNewProjectRemoteId: vi.fn(),
    newProjectSelectedRemote: null,
    newProjectUsesLocalRemote: true,
    newProjectRootPath: "",
    onChangeNewProjectRootPath: vi.fn(),
    handlePickProjectRoot: vi.fn(async () => {}),
    isSettingsOpen: false,
    closeSettingsDialog: vi.fn(),
    settingsTab,
    setSettingsTab: vi.fn(),
    activeStyle: STYLES[0],
    activeTheme: THEMES[0],
    styleId: STYLES[0].id,
    themeId: THEMES[0].id,
    setStyleId: vi.fn(),
    setThemeId: vi.fn(),
    activeMarkdownTheme: MARKDOWN_THEMES[0],
    activeMarkdownStyle: MARKDOWN_STYLES[0],
    markdownThemeId: MARKDOWN_THEMES[0].id,
    markdownStyleId: MARKDOWN_STYLES[0].id,
    diagramThemeOverrideMode: "on",
    diagramLook: DIAGRAM_LOOKS[0].id,
    diagramPalette: DIAGRAM_PALETTES[0].id,
    setMarkdownThemeId: vi.fn(),
    setMarkdownStyleId: vi.fn(),
    setDiagramThemeOverrideMode: vi.fn(),
    setDiagramLook: vi.fn(),
    setDiagramPalette: vi.fn(),
    densityPercent: 100,
    editorFontSizePx: 14,
    fontSizePx: 14,
    setDensityPercent: vi.fn(),
    setEditorFontSizePx: vi.fn(),
    setFontSizePx: vi.fn(),
    remoteConfigs: [],
    onSaveRemotes: vi.fn(),
    projects: [],
    sessions: [],
    handleOrchestratorStateUpdated: vi.fn(),
    defaultCodexApprovalPolicy: APPROVAL_POLICY_OPTIONS[0].value,
    defaultCodexSandboxMode: SANDBOX_MODE_OPTIONS[0].value,
    setDefaultCodexApprovalPolicy: vi.fn(),
    setDefaultCodexSandboxMode: vi.fn(),
    defaultClaudeApprovalMode: CLAUDE_APPROVAL_OPTIONS[0].value,
    setDefaultClaudeApprovalMode: vi.fn(),
    ...overrides,
  };
}

function getBackdrop(dialogName: string): HTMLElement {
  const dialog = screen.getByRole("dialog", { name: dialogName });
  const backdrop = dialog.parentElement;
  if (!(backdrop instanceof HTMLElement)) {
    throw new Error("Dialog backdrop not found.");
  }
  return backdrop;
}

function renderCreateSessionDialog(overrides: Partial<ComponentProps<typeof AppDialogs>> = {}) {
  const onClose = vi.fn();
  render(
    <AppDialogs
      {...createBaseProps({
        isCreateSessionOpen: true,
        closeCreateSessionDialog: onClose,
        ...overrides,
      })}
    />,
  );
  return onClose;
}

function renderCreateProjectDialog(overrides: Partial<ComponentProps<typeof AppDialogs>> = {}) {
  const onClose = vi.fn();
  render(
    <AppDialogs
      {...createBaseProps({
        isCreateProjectOpen: true,
        closeCreateProjectDialog: onClose,
        ...overrides,
      })}
    />,
  );
  return onClose;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

function renderSettingsDialog(
  settingsTab: PreferencesTabId,
  overrides: Partial<ComponentProps<typeof AppDialogs>> = {},
) {
  const onClose = vi.fn();
  render(
    <AppDialogs
      {...createBaseProps({
        isSettingsOpen: true,
        closeSettingsDialog: onClose,
        settingsTab,
        ...overrides,
      })}
    />,
  );
  return onClose;
}

describe("AppDialogs create-dialog backdrop dismissal", () => {
  let originalPlatform: PropertyDescriptor | undefined;
  let originalUserAgentData: PropertyDescriptor | undefined;

  beforeEach(() => {
    originalPlatform = Object.getOwnPropertyDescriptor(navigator, "platform");
    originalUserAgentData = Object.getOwnPropertyDescriptor(
      navigator as NavigatorWithUserAgentData,
      "userAgentData",
    );
    stubPlatform("Win32");
  });

  afterEach(() => {
    if (originalPlatform) {
      Object.defineProperty(navigator, "platform", originalPlatform);
    } else {
      Reflect.deleteProperty(navigator, "platform");
    }
    if (originalUserAgentData) {
      Object.defineProperty(navigator as NavigatorWithUserAgentData, "userAgentData", originalUserAgentData);
    } else {
      Reflect.deleteProperty(navigator as NavigatorWithUserAgentData, "userAgentData");
    }
  });

  function stubPlatform(platform: string) {
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: platform,
    });
    Object.defineProperty(navigator as NavigatorWithUserAgentData, "userAgentData", {
      configurable: true,
      value: { platform },
    });
  }

  it("closes the create-session dialog on primary-button backdrop mousedown when idle", () => {
    const onClose = renderCreateSessionDialog();

    fireEvent.mouseDown(getBackdrop("New session"), { button: 0 });

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not close the create-session dialog on non-dismiss backdrop gestures", () => {
    const onClose = renderCreateSessionDialog();

    fireEvent.mouseDown(getBackdrop("New session"), { button: 1 });
    fireEvent.mouseDown(getBackdrop("New session"), { button: 2 });
    stubPlatform("MacIntel");
    fireEvent.mouseDown(getBackdrop("New session"), { button: 0, ctrlKey: true });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("keeps the create-session dialog open while a create request is pending", () => {
    const onClose = renderCreateSessionDialog({ isCreating: true });

    fireEvent.mouseDown(getBackdrop("New session"), { button: 0 });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes the create-project dialog on primary-button backdrop mousedown when idle", () => {
    const onClose = renderCreateProjectDialog();

    fireEvent.mouseDown(getBackdrop("Add project"), { button: 0 });

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not close the create-project dialog on non-dismiss backdrop gestures", () => {
    const onClose = renderCreateProjectDialog();

    fireEvent.mouseDown(getBackdrop("Add project"), { button: 1 });
    fireEvent.mouseDown(getBackdrop("Add project"), { button: 2 });
    stubPlatform("MacIntel");
    fireEvent.mouseDown(getBackdrop("Add project"), { button: 0, ctrlKey: true });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("keeps the create-project dialog open while a create request is pending", () => {
    const onClose = renderCreateProjectDialog({ isCreatingProject: true });

    fireEvent.mouseDown(getBackdrop("Add project"), { button: 0 });

    expect(onClose).not.toHaveBeenCalled();
  });
});

describe("AppDialogs settings agent defaults", () => {
  it("keeps the settings scroll observers stable across same-tab rerenders", () => {
    const observerInstances: Array<{
      disconnect: ReturnType<typeof vi.fn>;
      observe: ReturnType<typeof vi.fn>;
    }> = [];
    class ResizeObserverMock {
      readonly disconnect = vi.fn();
      readonly observe = vi.fn();

      constructor() {
        observerInstances.push(this);
      }
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);

    const props = createBaseProps({
      isSettingsOpen: true,
      settingsTab: "appearance",
    });
    const { rerender, unmount } = render(<AppDialogs {...props} />);
    const initialObserverCount = observerInstances.length;

    expect(initialObserverCount).toBeGreaterThan(0);
    rerender(
      <AppDialogs
        {...props}
        densityPercent={props.densityPercent + 1}
      />,
    );

    expect(observerInstances).toHaveLength(initialObserverCount);
    expect(
      observerInstances.some((observer) => observer.disconnect.mock.calls.length > 0),
    ).toBe(false);

    rerender(<AppDialogs {...props} settingsTab="themes" />);

    expect(observerInstances.length).toBeGreaterThan(initialObserverCount);
    expect(
      observerInstances.some((observer) => observer.disconnect.mock.calls.length > 0),
    ).toBe(true);

    unmount();

    expect(
      observerInstances.filter((observer) => observer.disconnect.mock.calls.length > 0)
        .length,
    ).toBe(observerInstances.length);
  });

  it("renders the Cursor settings tab", () => {
    renderSettingsDialog("cursor");

    expect(
      screen.getByRole("heading", { name: "Cursor startup settings" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Default model")).toHaveAttribute(
      "id",
      "default-cursor-model",
    );
    expect(screen.getByLabelText("Default Cursor mode")).toBeInTheDocument();
    expect(screen.queryByLabelText("Default Gemini approvals")).toBeNull();
  });

  it("renders the Gemini settings tab", () => {
    renderSettingsDialog("gemini");

    expect(
      screen.getByRole("heading", { name: "Gemini startup settings" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Default model")).toHaveAttribute(
      "id",
      "default-gemini-model",
    );
    expect(screen.getByLabelText("Default Gemini approvals")).toBeInTheDocument();
    expect(screen.queryByLabelText("Default Cursor mode")).toBeNull();
  });

  it("renders the Telegram settings tab and fetches initial status", async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => {
      const status = {
        configured: true,
        enabled: true,
        running: false,
        lifecycle: "inProcess",
        linkedChatId: null,
        botTokenMasked: "saved-token",
        subscribedProjectIds: [],
        defaultProjectId: null,
        defaultSessionId: null,
      };
      return new Response(JSON.stringify(status), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    });
    vi.stubGlobal("fetch", fetchMock);

    renderSettingsDialog("telegram");

    expect(screen.getByRole("heading", { name: "Telegram" })).toBeInTheDocument();
    expect(screen.getByLabelText("Bot token")).toBeInTheDocument();
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/telegram/status");
    expect(await screen.findByText("Stopped")).toBeInTheDocument();
    expect(screen.getByLabelText("Bot token")).toHaveAttribute(
      "placeholder",
      "saved-token",
    );
  });
});
