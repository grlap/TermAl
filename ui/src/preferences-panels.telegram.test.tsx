import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchTelegramStatus,
  testTelegramConnection,
  updateTelegramConfig,
  type TelegramStatusResponse,
} from "./api";
import { TelegramPreferencesPanel } from "./preferences-panels";
import type { Project, Session } from "./types";

vi.mock("./api", async () => {
  const actual = await vi.importActual<typeof import("./api")>("./api");
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
  running: false,
  lifecycle: "manual",
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
        botToken: "123456:token",
        subscribedProjectIds: ["project-1"],
        defaultProjectId: null,
        defaultSessionId: null,
      });
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

  it.each([
    [
      "manual lifecycle",
      {
        configured: true,
        enabled: true,
        lifecycle: "manual" as const,
        botTokenMasked: "****oken",
      },
      "Configured",
    ],
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
