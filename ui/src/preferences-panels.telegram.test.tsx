import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
      screen.getByText(/No separate TermAl process or telegram command should be required/),
    ).toBeInTheDocument();

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

    await screen.findByText("Saved as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));

    await waitFor(() => {
      expect(testTelegramConnectionMock).toHaveBeenCalledWith({
        useSavedToken: true,
      });
    });
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

    await screen.findByText("Saved as ****oken.");
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
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    const { unmount } = render(
      <TelegramPreferencesPanel projects={projects} sessions={sessions} />,
    );

    await screen.findByText("Saved as ****oken.");
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    unmount();
    await act(async () => {
      testResult.resolve({
        botName: "TermAl Bot",
        botUsername: "termal_bot",
      });
      await testResult.promise;
    });

    expect(consoleError).not.toHaveBeenCalled();
    consoleError.mockRestore();
  });
});
