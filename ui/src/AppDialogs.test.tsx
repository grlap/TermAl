import { fireEvent, render, screen } from "@testing-library/react";
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
    defaultCodexReasoningEffort,
    handleDefaultCodexReasoningEffortChange: vi.fn(),
    defaultClaudeEffort,
    handleDefaultClaudeEffortChange: vi.fn(),
    defaultCursorMode: CURSOR_MODE_OPTIONS[0].value,
    onChangeDefaultCursorMode: vi.fn(),
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
