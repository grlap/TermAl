import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchTelegramStatus,
  testTelegramConnection,
  updateTelegramConfig,
  type TelegramStatusResponse,
} from "../api";
import type { Project, Session, TelegramUiConfig } from "../types";
import { TelegramPreferencesPanel } from "./telegram-preferences-panel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    fetchTelegramStatus: vi.fn(),
    testTelegramConnection: vi.fn(),
    updateTelegramConfig: vi.fn(),
  };
});

const fetchTelegramStatusMock = vi.mocked(fetchTelegramStatus);
const testTelegramConnectionMock = vi.mocked(testTelegramConnection);
const updateTelegramConfigMock = vi.mocked(updateTelegramConfig);

const emptyTelegramStatus: TelegramStatusResponse = {
  configured: false,
  enabled: false,
  forwardAssistantReplies: false,
  running: false,
  lifecycle: "inProcess",
  linkedChatId: null,
  botTokenMasked: null,
  subscribedProjectIds: [],
  defaultProjectId: null,
  defaultSessionId: null,
};

const projects: Project[] = [
  {
    id: "project-1",
    name: "TermAl",
    rootPath: "C:\\github\\Personal\\TermAl",
  },
];

const sessions: Session[] = [
  {
    id: "session-1",
    name: "Codex Live",
    emoji: "CX",
    agent: "Codex",
    workdir: "C:\\github\\Personal\\TermAl",
    projectId: "project-1",
    model: "gpt-5.4",
    status: "idle",
    preview: "",
    messages: [],
  },
];

function createDeferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

async function selectComboboxOption(name: string, optionName: string | RegExp) {
  fireEvent.click(await screen.findByRole("combobox", { name }));

  const listbox = await screen.findByRole("listbox");
  const option = within(listbox)
    .getAllByRole("option")
    .find((candidate) => {
      const label =
        candidate.querySelector(".combo-option-label")?.textContent?.trim() ??
        candidate.textContent?.trim() ??
        "";

      return typeof optionName === "string"
        ? label === optionName
        : optionName.test(label);
    });

  if (!option) {
    throw new Error(`Combobox option not found for ${String(optionName)}`);
  }

  fireEvent.click(option);
}

describe("TelegramPreferencesPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    fetchTelegramStatusMock.mockResolvedValue(emptyTelegramStatus);
    testTelegramConnectionMock.mockResolvedValue({
      botName: "TermAl Bot",
      botUsername: "termal_bot",
    });
    updateTelegramConfigMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      botTokenMasked: "****oken",
      subscribedProjectIds: ["project-1"],
    });
  });

  it("shows setup guidance and saves the selected Telegram settings", async () => {
    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    const tokenInput = await screen.findByLabelText("Bot token");
    fireEvent.click(screen.getByRole("button", { name: "Setup" }));
    expect(
      screen.getByText(/No separate TermAl process or telegram command is required/),
    ).toBeInTheDocument();
    expect(screen.getByText(/\/projects and \/project <id>/)).toBeInTheDocument();

    fireEvent.change(tokenInput, { target: { value: "123456:token" } });
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    await waitFor(() => {
      expect(testTelegramConnectionMock).toHaveBeenCalledWith({
        botToken: "123456:token",
      });
    });
    expect(await screen.findByText("Connected to @termal_bot.")).toBeInTheDocument();

    const projectList = screen.getByLabelText("Telegram subscribed projects");
    fireEvent.click(within(projectList).getByLabelText(/TermAl/));
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));

    await waitFor(() => {
      expect(updateTelegramConfigMock).toHaveBeenCalledWith({
        enabled: false,
        forwardAssistantReplies: false,
        botToken: "123456:token",
        subscribedProjectIds: ["project-1"],
        defaultProjectId: null,
        defaultSessionId: null,
      });
    });
  });

  it("warns before saving assistant-reply forwarding to Telegram", async () => {
    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    await screen.findByText(/Sends full assistant output to Telegram/);
    const forwardingToggle = screen.getByLabelText("Forward assistant replies");
    expect(forwardingToggle).toHaveAccessibleDescription(
      /including any code, file paths, file contents, or secrets/,
    );
    fireEvent.click(forwardingToggle);
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));

    await waitFor(() => {
      expect(updateTelegramConfigMock).toHaveBeenCalledWith(
        expect.objectContaining({
          forwardAssistantReplies: true,
        }),
      );
    });
  });

  it("explicitly opts into testing the saved Telegram token", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      botTokenMasked: "****oken",
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    await screen.findByText("Stored in the OS credential store as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));

    await waitFor(() => {
      expect(testTelegramConnectionMock).toHaveBeenCalledWith({
        useSavedToken: true,
      });
    });
  });

  it("labels a configured in-process relay as stopped when it is not polling", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      enabled: true,
      lifecycle: "inProcess",
      botTokenMasked: "****oken",
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    expect(await screen.findByText("Stopped")).toBeInTheDocument();
  });

  it("labels stopped in-process relays before linked chat state", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      enabled: true,
      lifecycle: "inProcess",
      linkedChatId: 123,
      botTokenMasked: "****oken",
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    expect(await screen.findByText("Stopped")).toBeInTheDocument();
    expect(screen.queryByText("Linked")).not.toBeInTheDocument();
    expect(screen.getByText("Linked chat id: 123")).toBeInTheDocument();
  });

  it.each([
    [
      "disabled in-process relay",
      {
        configured: true,
        enabled: false,
        lifecycle: "inProcess" as const,
        botTokenMasked: "****oken",
      },
      "Configured",
    ],
    [
      "not configured in-process relay",
      {
        configured: false,
        enabled: true,
        lifecycle: "inProcess" as const,
      },
      "Not configured",
    ],
    [
      "running in-process relay",
      {
        configured: true,
        enabled: true,
        running: true,
        lifecycle: "inProcess" as const,
        botTokenMasked: "****oken",
      },
      "Polling",
    ],
  ])("does not label %s as stopped", async (_caseName, status, expectedLabel) => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      ...status,
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    expect(await screen.findByText(expectedLabel)).toBeInTheDocument();
    expect(screen.queryByText("Stopped")).not.toBeInTheDocument();
  });

  it("does not save an enabled relay without a subscribed project", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      enabled: true,
      lifecycle: "inProcess",
      botTokenMasked: "****oken",
      subscribedProjectIds: [],
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    await screen.findByText("Stored in the OS credential store as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));

    expect(
      await screen.findByText("Choose at least one Telegram project before enabling the relay."),
    ).toBeInTheDocument();
    expect(updateTelegramConfigMock).not.toHaveBeenCalled();
  });

  it("surfaces Telegram save API errors", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      botTokenMasked: "****oken",
    });
    updateTelegramConfigMock.mockRejectedValueOnce(new Error("Telegram save failed."));

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    await screen.findByText("Stored in the OS credential store as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));

    expect(await screen.findByText("Telegram save failed.")).toBeInTheDocument();
  });

  it("adopts a backend-normalized disabled response after an enabled save", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      enabled: false,
      lifecycle: "inProcess",
      botTokenMasked: "****oken",
      subscribedProjectIds: ["project-1"],
    });
    updateTelegramConfigMock.mockResolvedValueOnce({
      ...emptyTelegramStatus,
      configured: true,
      enabled: false,
      lifecycle: "inProcess",
      botTokenMasked: "****oken",
      subscribedProjectIds: [],
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    await screen.findByText("Stored in the OS credential store as ****oken.");
    fireEvent.click(screen.getByLabelText("Enable relay"));
    expect(screen.getByLabelText("Enable relay")).toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));

    await waitFor(() => {
      expect(updateTelegramConfigMock).toHaveBeenCalledWith({
        enabled: true,
        forwardAssistantReplies: false,
        botToken: undefined,
        subscribedProjectIds: ["project-1"],
        defaultProjectId: null,
        defaultSessionId: null,
      });
    });
    expect(await screen.findByText("Telegram settings saved.")).toBeInTheDocument();
    expect(screen.getByText("Configured")).toBeInTheDocument();
    expect(screen.getByLabelText("Enable relay")).not.toBeChecked();
    expect(
      within(screen.getByLabelText("Telegram subscribed projects")).getByLabelText(
        /TermAl/,
      ),
    ).not.toBeChecked();
  });

  it("auto-subscribes the selected default project when saving", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      botTokenMasked: "****oken",
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    await screen.findByText("Stored in the OS credential store as ****oken.");
    await selectComboboxOption("Default project", "TermAl");
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));

    await waitFor(() => {
      expect(updateTelegramConfigMock).toHaveBeenCalledWith({
        enabled: false,
        forwardAssistantReplies: false,
        botToken: undefined,
        subscribedProjectIds: ["project-1"],
        defaultProjectId: "project-1",
        defaultSessionId: null,
      });
    });
  });

  it("clears a stale default session when saving", async () => {
    const staleTelegramConfig: TelegramUiConfig = {
      enabled: true,
      forwardAssistantReplies: false,
      subscribedProjectIds: ["project-1"],
      defaultProjectId: "project-1",
      defaultSessionId: "missing-session",
    };
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      botTokenMasked: "****oken",
    });

    render(
      <TelegramPreferencesPanel
        telegramConfig={staleTelegramConfig}
        projects={projects}
        sessions={sessions}
      />,
    );

    await screen.findByText("Stored in the OS credential store as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));

    await waitFor(() => {
      expect(updateTelegramConfigMock).toHaveBeenCalledWith({
        enabled: true,
        forwardAssistantReplies: false,
        botToken: undefined,
        subscribedProjectIds: ["project-1"],
        defaultProjectId: "project-1",
        defaultSessionId: null,
      });
    });
  });

  it("trusts the status response before app-state Telegram config hydrates", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      enabled: true,
      botTokenMasked: "****oken",
      subscribedProjectIds: ["project-1"],
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    expect(await screen.findByText("Stopped")).toBeInTheDocument();
    expect(screen.getByLabelText("Enable relay")).toBeChecked();
    expect(
      within(screen.getByLabelText("Telegram subscribed projects")).getByLabelText(
        /TermAl/,
      ),
    ).toBeChecked();
  });

  it("adopts app-state Telegram config updates while already open", async () => {
    const { rerender } = render(
      <TelegramPreferencesPanel projects={projects} sessions={sessions} />,
    );

    expect(await screen.findByText("Not configured")).toBeInTheDocument();
    expect(screen.getByLabelText("Enable relay")).not.toBeChecked();

    const nextTelegramConfig: TelegramUiConfig = {
      enabled: true,
      forwardAssistantReplies: true,
      subscribedProjectIds: ["project-1"],
      defaultProjectId: "project-1",
      defaultSessionId: "session-1",
    };
    rerender(
      <TelegramPreferencesPanel
        telegramConfig={nextTelegramConfig}
        projects={projects}
        sessions={sessions}
      />,
    );

    expect(screen.getByLabelText("Enable relay")).toBeChecked();
    expect(screen.getByLabelText("Forward assistant replies")).toBeChecked();
    expect(
      within(screen.getByLabelText("Telegram subscribed projects")).getByLabelText(
        /TermAl/,
      ),
    ).toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));

    await waitFor(() => {
      expect(updateTelegramConfigMock).toHaveBeenCalledWith({
        enabled: true,
        forwardAssistantReplies: true,
        botToken: undefined,
        subscribedProjectIds: ["project-1"],
        defaultProjectId: "project-1",
        defaultSessionId: "session-1",
      });
    });
  });

  it("refreshes runtime status after an app-state Telegram config change", async () => {
    const initialTelegramConfig: TelegramUiConfig = {
      enabled: false,
      forwardAssistantReplies: false,
      subscribedProjectIds: [],
      defaultProjectId: null,
      defaultSessionId: null,
    };
    fetchTelegramStatusMock
      .mockResolvedValueOnce({
        ...emptyTelegramStatus,
        configured: true,
        botTokenMasked: "****oken",
      })
      .mockResolvedValueOnce({
        ...emptyTelegramStatus,
        configured: true,
        running: true,
        botTokenMasked: "****oken",
      });

    const { rerender } = render(
      <TelegramPreferencesPanel
        telegramConfig={initialTelegramConfig}
        projects={projects}
        sessions={sessions}
      />,
    );

    expect(await screen.findByText("Configured")).toBeInTheDocument();
    rerender(
      <TelegramPreferencesPanel
        telegramConfig={{
          ...initialTelegramConfig,
          enabled: true,
          subscribedProjectIds: ["project-1"],
        }}
        projects={projects}
        sessions={sessions}
      />,
    );

    await waitFor(() => expect(fetchTelegramStatusMock).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("Polling")).toBeInTheDocument();
  });

  it("clears loading when app-state runtime refresh supersedes the initial status fetch", async () => {
    const initialStatus = createDeferred<TelegramStatusResponse>();
    const refreshStatus = createDeferred<TelegramStatusResponse>();
    const initialTelegramConfig: TelegramUiConfig = {
      enabled: false,
      forwardAssistantReplies: false,
      subscribedProjectIds: [],
      defaultProjectId: null,
      defaultSessionId: null,
    };
    fetchTelegramStatusMock
      .mockReturnValueOnce(initialStatus.promise)
      .mockReturnValueOnce(refreshStatus.promise);

    const { rerender } = render(
      <TelegramPreferencesPanel
        telegramConfig={initialTelegramConfig}
        projects={projects}
        sessions={sessions}
      />,
    );

    rerender(
      <TelegramPreferencesPanel
        telegramConfig={{
          ...initialTelegramConfig,
          enabled: true,
          subscribedProjectIds: ["project-1"],
        }}
        projects={projects}
        sessions={sessions}
      />,
    );

    await waitFor(() => expect(fetchTelegramStatusMock).toHaveBeenCalledTimes(2));
    await act(async () => {
      refreshStatus.resolve({
        ...emptyTelegramStatus,
        configured: true,
        botTokenMasked: "****oken",
      });
      await refreshStatus.promise;
    });

    expect(screen.getByRole("button", { name: "Save Telegram" })).toBeEnabled();
    await act(async () => {
      initialStatus.resolve({
        ...emptyTelegramStatus,
        configured: true,
        enabled: false,
        botTokenMasked: "****stale",
      });
      await initialStatus.promise;
    });
    expect(screen.getByRole("button", { name: "Save Telegram" })).toBeEnabled();
  });

  it("ignores stale app-state runtime refreshes after saving newer local edits", async () => {
    const refreshStatus = createDeferred<TelegramStatusResponse>();
    const initialTelegramConfig: TelegramUiConfig = {
      enabled: false,
      forwardAssistantReplies: false,
      subscribedProjectIds: [],
      defaultProjectId: null,
      defaultSessionId: null,
    };
    fetchTelegramStatusMock
      .mockResolvedValueOnce(emptyTelegramStatus)
      .mockReturnValueOnce(refreshStatus.promise);
    updateTelegramConfigMock.mockResolvedValueOnce({
      ...emptyTelegramStatus,
      configured: true,
      enabled: false,
      botTokenMasked: "****oken",
    });

    const { rerender } = render(
      <TelegramPreferencesPanel
        telegramConfig={initialTelegramConfig}
        projects={projects}
        sessions={sessions}
      />,
    );

    expect(await screen.findByText("Not configured")).toBeInTheDocument();
    rerender(
      <TelegramPreferencesPanel
        telegramConfig={{
          ...initialTelegramConfig,
          enabled: true,
          subscribedProjectIds: ["project-1"],
        }}
        projects={projects}
        sessions={sessions}
      />,
    );

    await waitFor(() => expect(fetchTelegramStatusMock).toHaveBeenCalledTimes(2));
    fireEvent.click(screen.getByLabelText("Enable relay"));
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));
    await waitFor(() => {
      expect(updateTelegramConfigMock).toHaveBeenCalledWith({
        enabled: false,
        forwardAssistantReplies: false,
        botToken: undefined,
        subscribedProjectIds: ["project-1"],
        defaultProjectId: null,
        defaultSessionId: null,
      });
    });
    expect(await screen.findByText("Telegram settings saved.")).toBeInTheDocument();

    await act(async () => {
      refreshStatus.resolve({
        ...emptyTelegramStatus,
        configured: true,
        enabled: true,
        running: true,
        botTokenMasked: "****stale",
        subscribedProjectIds: ["project-1"],
      });
      await refreshStatus.promise;
    });

    expect(screen.getByLabelText("Enable relay")).not.toBeChecked();
    expect(screen.queryByText("Polling")).not.toBeInTheDocument();
  });

  it("surfaces app-state runtime refresh failures without reverting adopted config", async () => {
    const initialTelegramConfig: TelegramUiConfig = {
      enabled: false,
      forwardAssistantReplies: false,
      subscribedProjectIds: [],
      defaultProjectId: null,
      defaultSessionId: null,
    };
    fetchTelegramStatusMock
      .mockResolvedValueOnce(emptyTelegramStatus)
      .mockRejectedValueOnce(new Error("Runtime status unavailable."));

    const { rerender } = render(
      <TelegramPreferencesPanel
        telegramConfig={initialTelegramConfig}
        projects={projects}
        sessions={sessions}
      />,
    );

    expect(await screen.findByText("Not configured")).toBeInTheDocument();
    rerender(
      <TelegramPreferencesPanel
        telegramConfig={{
          ...initialTelegramConfig,
          enabled: true,
          subscribedProjectIds: ["project-1"],
        }}
        projects={projects}
        sessions={sessions}
      />,
    );

    expect(await screen.findByText("Runtime status unavailable.")).toBeInTheDocument();
    expect(screen.getByLabelText("Enable relay")).toBeChecked();
  });

  it("keeps unsaved local edits when app-state Telegram config changes", async () => {
    const initialTelegramConfig: TelegramUiConfig = {
      enabled: false,
      forwardAssistantReplies: false,
      subscribedProjectIds: [],
      defaultProjectId: null,
      defaultSessionId: null,
    };
    const { rerender } = render(
      <TelegramPreferencesPanel
        telegramConfig={initialTelegramConfig}
        projects={projects}
        sessions={sessions}
      />,
    );

    const tokenInput = await screen.findByLabelText("Bot token");
    fireEvent.change(tokenInput, { target: { value: "local-unsaved-token" } });
    rerender(
      <TelegramPreferencesPanel
        telegramConfig={{
          ...initialTelegramConfig,
          enabled: true,
          forwardAssistantReplies: true,
          subscribedProjectIds: ["project-1"],
        }}
        projects={projects}
        sessions={sessions}
      />,
    );

    expect(screen.getByLabelText("Bot token")).toHaveValue("local-unsaved-token");
    expect(screen.getByLabelText("Enable relay")).not.toBeChecked();
    expect(
      await screen.findByText(
        "Telegram settings changed elsewhere; unsaved edits were kept.",
      ),
    ).toBeInTheDocument();
  });

  it("keeps handler state updates enabled after React StrictMode remount checks", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      botTokenMasked: "****oken",
    });

    render(
      <StrictMode>
        <TelegramPreferencesPanel projects={projects} sessions={sessions} />
      </StrictMode>,
    );

    await screen.findByText("Stored in the OS credential store as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));

    expect(await screen.findByText("Connected to @termal_bot.")).toBeInTheDocument();
  });

  it("keeps save and remove updates enabled after React StrictMode remount checks", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      enabled: false,
      botTokenMasked: "****oken",
    });
    updateTelegramConfigMock
      .mockResolvedValueOnce({
        ...emptyTelegramStatus,
        configured: true,
        enabled: false,
        botTokenMasked: "****oken",
        subscribedProjectIds: ["project-1"],
      })
      .mockResolvedValueOnce(emptyTelegramStatus);

    render(
      <StrictMode>
        <TelegramPreferencesPanel projects={projects} sessions={sessions} />
      </StrictMode>,
    );

    await screen.findByText("Stored in the OS credential store as ****oken.");
    const projectList = screen.getByLabelText("Telegram subscribed projects");
    fireEvent.click(within(projectList).getByLabelText(/TermAl/));
    fireEvent.click(screen.getByRole("button", { name: "Save Telegram" }));
    expect(await screen.findByText("Telegram settings saved.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Remove token" }));
    expect(await screen.findByText("Telegram bot token removed.")).toBeInTheDocument();
  });

  it("ignores stale initial Telegram status fetches after React StrictMode remount checks", async () => {
    const staleStatus = createDeferred<TelegramStatusResponse>();
    fetchTelegramStatusMock
      .mockReturnValueOnce(staleStatus.promise)
      .mockResolvedValueOnce({
        ...emptyTelegramStatus,
        configured: true,
        botTokenMasked: "****fresh",
      });

    render(
      <StrictMode>
        <TelegramPreferencesPanel projects={projects} sessions={sessions} />
      </StrictMode>,
    );

    expect(
      await screen.findByText("Stored in the OS credential store as ****fresh."),
    ).toBeInTheDocument();

    await act(async () => {
      staleStatus.resolve({
        ...emptyTelegramStatus,
        configured: true,
        botTokenMasked: "****stale",
      });
      await staleStatus.promise;
    });

    expect(screen.getByText("Stored in the OS credential store as ****fresh."))
      .toBeInTheDocument();
    expect(screen.queryByText("Stored in the OS credential store as ****stale."))
      .not.toBeInTheDocument();
  });

  it("removes a saved Telegram token explicitly", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      enabled: true,
      botTokenMasked: "****oken",
      subscribedProjectIds: ["project-1"],
    });
    updateTelegramConfigMock.mockResolvedValueOnce(emptyTelegramStatus);

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    await screen.findByText("Stored in the OS credential store as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Remove token" }));

    await waitFor(() => {
      expect(updateTelegramConfigMock).toHaveBeenCalledWith({
        enabled: false,
        botToken: null,
      });
    });
    expect(await screen.findByText("Telegram bot token removed.")).toBeInTheDocument();
  });

  it("does not update state after an in-flight Telegram test unmounts", async () => {
    fetchTelegramStatusMock.mockResolvedValue({
      ...emptyTelegramStatus,
      configured: true,
      botTokenMasked: "****oken",
    });
    const testResult = createDeferred<Awaited<ReturnType<typeof testTelegramConnection>>>();
    testTelegramConnectionMock.mockReturnValueOnce(testResult.promise);

    const { unmount } = render(
      <TelegramPreferencesPanel projects={projects} sessions={sessions} />,
    );

    await screen.findByText("Stored in the OS credential store as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    unmount();
    await act(async () => {
      testResult.resolve({
        botName: "TermAl Bot",
        botUsername: "termal_bot",
      });
      await testResult.promise;
    });

    render(<TelegramPreferencesPanel projects={projects} sessions={sessions} />);

    expect(
      await screen.findByText("Stored in the OS credential store as ****oken."),
    ).toBeInTheDocument();
    expect(screen.queryByText("Connected to @termal_bot.")).not.toBeInTheDocument();
  });
});
