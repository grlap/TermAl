import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ComponentProps } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  registerRemoteTermal,
  upgradeRemoteTermal,
} from "./api";
import {
  ALL_CLAUDE_APPROVAL_MODES,
  ClaudeApprovalsPreferencesPanel,
  CLAUDE_APPROVAL_OPTIONS,
  CodexPromptPreferencesPanel,
  CursorPreferencesPanel,
  GeminiPreferencesPanel,
  INTERNAL_CLAUDE_APPROVAL_MODES,
  OpenCodePreferencesPanel,
  RemotePreferencesPanel,
  ThemePreferencesPanel,
} from "./preferences-panels";
import { THEMES } from "./themes";
import type { RemoteConfig } from "./types";

vi.mock("./api", async () => {
  const actual = await vi.importActual<typeof import("./api")>("./api");
  return {
    ...actual,
    registerRemoteTermal: vi.fn(),
    upgradeRemoteTermal: vi.fn(),
  };
});

const registerRemoteTermalMock = vi.mocked(registerRemoteTermal);
const upgradeRemoteTermalMock = vi.mocked(upgradeRemoteTermal);

describe("ThemePreferencesPanel", () => {
  it("renders mode, paired slots, and the theme catalog as distinct visual sections", () => {
    const { container } = render(
      <ThemePreferencesPanel
        activeStyle={{
          id: "theme-default",
          name: "Match Theme",
          description: "Use the visual treatment bundled with the selected theme.",
        }}
        activeTheme={THEMES.find((theme) => theme.id === "warm-light")!}
        activeThemeKind="light"
        darkThemeId="dark"
        lightThemeId="warm-light"
        styleId="theme-default"
        themeMode="light"
        themeSessionOverride={null}
        onReturnToAuto={vi.fn()}
        onSelectMode={vi.fn()}
        onSelectStyle={vi.fn()}
        onSelectTheme={vi.fn()}
      />,
    );

    expect(screen.getByRole("heading", { name: "Mode" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Your pair" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Light themes" })).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Theme mode" })).toHaveClass(
      "theme-mode-segmented",
    );
    expect(screen.getByRole("button", { name: "Light" })).toHaveTextContent("☀︎");
    expect(screen.getByRole("button", { name: "Dark" })).toHaveTextContent("☾");
    expect(screen.getByRole("article", { name: "Light theme: Warm Light" })).toBeInTheDocument();
    expect(screen.getByRole("article", { name: "Dark theme: Darkroom" })).toBeInTheDocument();
    expect(screen.getByText("Active")).toBeInTheDocument();
    expect(screen.queryByRole("group", { name: "Theme filter" })).toBeNull();
    expect(screen.getByRole("group", { name: "UI themes" })).toHaveClass(
      "theme-catalog-grid",
    );
    expect(container.querySelectorAll(".theme-swatch-preview").length).toBeGreaterThan(2);
    expect(container.querySelector(".theme-option-scrollbar")).toBeNull();
  });

  it("derives the catalog filter from mode and assigns a clicked theme to its matching slot", () => {
    const onSelectTheme = vi.fn();

    const panelProps: Omit<
      ComponentProps<typeof ThemePreferencesPanel>,
      "themeMode"
    > = {
      activeStyle: {
        id: "theme-default",
        name: "Match Theme",
        description: "Use the visual treatment bundled with the selected theme.",
      },
      activeTheme: THEMES.find((theme) => theme.id === "warm-light")!,
      activeThemeKind: "light",
      darkThemeId: "dark",
      lightThemeId: "warm-light",
      styleId: "theme-default",
      themeSessionOverride: null,
      onReturnToAuto: vi.fn(),
      onSelectMode: vi.fn(),
      onSelectStyle: vi.fn(),
      onSelectTheme,
    };
    const { rerender } = render(
      <ThemePreferencesPanel
        {...panelProps}
        themeMode="light"
      />,
    );

    expect(screen.getByRole("button", { name: /Warm Light/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Darkroom/i })).toBeNull();

    rerender(<ThemePreferencesPanel {...panelProps} themeMode="dark" />);

    expect(screen.getByRole("heading", { name: "Dark themes" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Darkroom/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Warm Light/i })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /Darkroom/i }));
    expect(onSelectTheme).toHaveBeenCalledWith("dark");

    rerender(<ThemePreferencesPanel {...panelProps} themeMode="auto" />);

    expect(screen.getByRole("heading", { name: "All themes" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Warm Light/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Darkroom/i })).toBeInTheDocument();
    const catalog = screen.getByRole("group", { name: "UI themes" });
    expect(catalog.querySelectorAll('button[aria-pressed="true"]')).toHaveLength(2);
  });
});

function renderCodexPanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof CodexPromptPreferencesPanel>["sessions"];
} = {}) {
  const props = {
    defaultApprovalPolicy: "never" as const,
    defaultModel,
    defaultReasoningEffort: "medium" as const,
    defaultSandboxMode: "workspace-write" as const,
    onSelectApprovalPolicy: vi.fn(),
    onSelectModel,
    onSelectReasoningEffort: vi.fn(),
    onSelectSandboxMode: vi.fn(),
    sessions,
  };

  return {
    onSelectModel,
    ...render(<CodexPromptPreferencesPanel {...props} />),
  };
}

function renderClaudePanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof ClaudeApprovalsPreferencesPanel>["sessions"];
} = {}) {
  const props = {
    defaultClaudeApprovalMode: "ask" as const,
    defaultClaudeEffort: "default" as const,
    defaultClaudeModel: defaultModel,
    onSelectEffort: vi.fn(),
    onSelectModel,
    onSelectMode: vi.fn(),
    sessions,
  };

  return {
    onSelectModel,
    ...render(<ClaudeApprovalsPreferencesPanel {...props} />),
  };
}

function renderCursorPanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof CursorPreferencesPanel>["sessions"];
} = {}) {
  const props = {
    defaultCursorMode: "agent" as const,
    defaultCursorModel: defaultModel,
    onSelectModel,
    onSelectMode: vi.fn(),
    sessions,
  };

  return {
    onSelectModel,
    ...render(<CursorPreferencesPanel {...props} />),
  };
}

function renderGeminiPanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof GeminiPreferencesPanel>["sessions"];
} = {}) {
  const props = {
    defaultGeminiApprovalMode: "default" as const,
    defaultGeminiModel: defaultModel,
    onSelectApprovalMode: vi.fn(),
    onSelectModel,
    sessions,
  };

  return {
    onSelectModel,
    ...render(<GeminiPreferencesPanel {...props} />),
  };
}

function renderOpenCodePanel({
  defaultModel = "default",
  onSelectModel = vi.fn(),
  sessions = [],
}: {
  defaultModel?: string;
  onSelectModel?: (model: string) => void;
  sessions?: ComponentProps<typeof OpenCodePreferencesPanel>["sessions"];
} = {}) {
  return {
    onSelectModel,
    ...render(
      <OpenCodePreferencesPanel
        defaultOpenCodeModel={defaultModel}
        onSelectModel={onSelectModel}
        sessions={sessions}
      />,
    ),
  };
}

function createRemote(overrides: Partial<RemoteConfig> = {}): RemoteConfig {
  return {
    id: "ssh-1",
    name: "SSH Remote 1",
    transport: "ssh",
    enabled: true,
    host: "10.0.0.178",
    port: 22,
    user: "greg",
    ...overrides,
  };
}

function createDeferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

describe("RemotePreferencesPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    registerRemoteTermalMock.mockResolvedValue({
      remoteId: "ssh-1",
      action: "registration",
      message: "remote `SSH Remote 1` registration completed",
      stdout: "registered TermAl checkout: /srv/TermAl",
    });
    upgradeRemoteTermalMock.mockResolvedValue({
      remoteId: "ssh-1",
      action: "upgrade",
      message: "remote `SSH Remote 1` upgrade completed",
      stdout: "updated TermAl from /srv/TermAl",
    });
  });

  it("disables remote lifecycle actions while remote settings have unsaved changes", () => {
    render(
      <RemotePreferencesPanel remotes={[createRemote()]} onSaveRemotes={vi.fn()} />,
    );

    expect(screen.getByRole("button", { name: "Register TermAl" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Build / upgrade" })).toBeEnabled();

    fireEvent.change(screen.getByLabelText("Host"), {
      target: { value: "10.0.0.179" },
    });

    expect(screen.getByRole("button", { name: "Register TermAl" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Build / upgrade" })).toBeDisabled();
  });

  it("registers remote TermAl with the checkout path and renders capped action output", async () => {
    render(
      <RemotePreferencesPanel remotes={[createRemote()]} onSaveRemotes={vi.fn()} />,
    );

    fireEvent.change(screen.getByLabelText("TermAl checkout path"), {
      target: { value: "/srv/TermAl" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Register TermAl" }));

    await waitFor(() => {
      expect(registerRemoteTermalMock).toHaveBeenCalledWith("ssh-1", {
        sourcePath: "/srv/TermAl",
      });
    });
    expect(
      await screen.findByText("remote `SSH Remote 1` registration completed"),
    ).toBeInTheDocument();
    expect(screen.getByText("registered TermAl checkout: /srv/TermAl")).toBeInTheDocument();
  });

  it("renders remote upgrade errors", async () => {
    upgradeRemoteTermalMock.mockRejectedValue(new Error("build failed"));
    render(
      <RemotePreferencesPanel remotes={[createRemote()]} onSaveRemotes={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Build / upgrade" }));

    await waitFor(() => {
      expect(upgradeRemoteTermalMock).toHaveBeenCalledWith("ssh-1");
    });
    expect(await screen.findByText("build failed")).toBeInTheDocument();
  });

  it("ignores remote action completions after unmount", async () => {
    const deferred = createDeferred<Awaited<ReturnType<typeof registerRemoteTermal>>>();
    registerRemoteTermalMock.mockReturnValue(deferred.promise);
    const rendered = render(
      <RemotePreferencesPanel remotes={[createRemote()]} onSaveRemotes={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Register TermAl" }));
    rendered.unmount();
    await act(async () => {
      deferred.resolve({
        remoteId: "ssh-1",
        action: "registration",
        message: "remote `SSH Remote 1` registration completed",
      });
      await deferred.promise;
    });

    expect(registerRemoteTermalMock).toHaveBeenCalledTimes(1);
  });
});

describe("AgentDefaultModelControl", () => {
  it("keeps configured custom values in the dropdown without a text input", () => {
    renderCodexPanel({ defaultModel: "gpt-5.5" });

    expect(
      screen.getByRole("combobox", { name: "Codex default model" }),
    ).toHaveTextContent("gpt-5.5");
    expect(
      screen.queryByLabelText("Codex custom default model"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Apply Codex default model" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/Select a known model/u)).toHaveTextContent(
      "Select a known model or choose Default to let Codex use its built-in default.",
    );
  });

  it("renders Claude default model selection as dropdown-only", () => {
    renderClaudePanel();

    expect(
      screen.getByRole("combobox", { name: "Claude default model" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Claude custom default model"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Apply Claude default model" }),
    ).not.toBeInTheDocument();
  });

  it("selects the canonical default sentinel from the combobox", async () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ defaultModel: "gpt-5.5", onSelectModel });

    fireEvent.click(screen.getByRole("combobox", { name: "Codex default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Default/u }));

    expect(onSelectModel).toHaveBeenCalledWith("default");
  });

  it("renders default-like upstream values as the canonical sentinel", () => {
    renderCodexPanel({ defaultModel: " DEFAULT " });

    expect(
      screen.getByRole("combobox", { name: "Codex default model" }),
    ).toHaveTextContent("Default");
  });

  it("offers static Codex model choices before a live session model list loads", async () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({ onSelectModel });

    fireEvent.click(screen.getByRole("combobox", { name: "Codex default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /GPT-5\.4/u }));

    expect(onSelectModel).toHaveBeenCalledWith("gpt-5.4");
  });

  it("selects a Codex default model from live session options", async () => {
    const onSelectModel = vi.fn();
    renderCodexPanel({
      onSelectModel,
      sessions: [
        {
          id: "codex-1",
          name: "Codex",
          emoji: "",
          agent: "Codex",
          workdir: "/tmp",
          model: "default",
          modelOptions: [
            {
              label: "GPT-5.5",
              value: "gpt-5.5",
              description: "Latest Codex model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "Codex default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /GPT-5\.5/u }));

    expect(onSelectModel).toHaveBeenCalledWith("gpt-5.5");
  });

  it("selects a Claude default model from live session options", async () => {
    const onSelectModel = vi.fn();
    renderClaudePanel({
      onSelectModel,
      sessions: [
        {
          id: "claude-1",
          name: "Claude",
          emoji: "",
          agent: "Claude",
          workdir: "/tmp",
          model: "default",
          modelOptions: [
            {
              label: "Claude Sonnet 4.5",
              value: "claude-sonnet-4-5",
              description: "Balanced Claude model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "Claude default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Claude Sonnet 4\.5/u }));

    expect(onSelectModel).toHaveBeenCalledWith("claude-sonnet-4-5");
  });

  it("selects Cursor, Gemini, and OpenCode defaults from live session options", async () => {
    const onSelectCursorModel = vi.fn();
    renderCursorPanel({
      onSelectModel: onSelectCursorModel,
      sessions: [
        {
          id: "cursor-1",
          name: "Cursor",
          emoji: "",
          agent: "Cursor",
          workdir: "/tmp",
          model: "auto",
          modelOptions: [
            {
              label: "Cursor Pro",
              value: "cursor-pro",
              description: "Cursor subscription model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "Cursor default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Cursor Pro/u }));

    expect(onSelectCursorModel).toHaveBeenCalledWith("cursor-pro");

    const onSelectGeminiModel = vi.fn();
    renderGeminiPanel({
      onSelectModel: onSelectGeminiModel,
      sessions: [
        {
          id: "gemini-1",
          name: "Gemini",
          emoji: "",
          agent: "Gemini",
          workdir: "/tmp",
          model: "auto",
          modelOptions: [
            {
              label: "Gemini Pro",
              value: "gemini-pro",
              description: "Gemini model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "Gemini default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /Gemini Pro/u }));

    expect(onSelectGeminiModel).toHaveBeenCalledWith("gemini-pro");

    const onSelectOpenCodeModel = vi.fn();
    renderOpenCodePanel({
      onSelectModel: onSelectOpenCodeModel,
      sessions: [
        {
          id: "opencode-1",
          name: "OpenCode",
          emoji: "",
          agent: "OpenCode",
          workdir: "/tmp",
          model: "opencode/big-pickle",
          opencodeModel: "auto",
          opencodeMode: "auto",
          modelOptions: [
            {
              label: "GPT-5.6 Sol",
              value: "openai/gpt-5.6-sol",
              description: "OpenCode provider model",
            },
          ],
          status: "idle",
          preview: "",
          messages: [],
        },
      ],
    });

    fireEvent.click(screen.getByRole("combobox", { name: "OpenCode default model" }));
    fireEvent.click(await screen.findByRole("option", { name: /GPT-5\.6 Sol/u }));

    expect(onSelectOpenCodeModel).toHaveBeenCalledWith("openai/gpt-5.6-sol");
  });

  it("keeps read-only auto-approve internal to delegation flows", () => {
    const userFacingModes = CLAUDE_APPROVAL_OPTIONS.map((option) => option.value);
    const internalModes = Array.from(INTERNAL_CLAUDE_APPROVAL_MODES);
    const partitionedModes = [...userFacingModes, ...internalModes].sort();

    expect(userFacingModes).not.toContain("read-only-auto-approve");
    expect(partitionedModes).toEqual([...ALL_CLAUDE_APPROVAL_MODES].sort());
    expect(new Set(partitionedModes).size).toBe(partitionedModes.length);
  });
});
