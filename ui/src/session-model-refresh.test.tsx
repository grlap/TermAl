import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { CursorPromptSettingsCard, GeminiPromptSettingsCard } from "./App";
import type { Session } from "./types";

function makeSession(id: string, overrides?: Partial<Session>): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Cursor",
    workdir: "/tmp",
    model: "auto",
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

describe("session model refresh controls", () => {
  it("auto-requests Cursor model options when the session card opens without a live list", async () => {
    const onRequestModelOptions = vi.fn();

    render(
      <CursorPromptSettingsCard
        paneId="pane-cursor"
        session={makeSession("cursor-session", {
          agent: "Cursor",
          cursorMode: "agent",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    await waitFor(() => {
      expect(onRequestModelOptions).toHaveBeenCalledWith("cursor-session");
    });
    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("button", { name: "Refresh models" }),
    ).toBeInTheDocument();
  });

  it("lets Gemini refresh model options manually from the session card", () => {
    const onRequestModelOptions = vi.fn();

    render(
      <GeminiPromptSettingsCard
        paneId="pane-gemini"
        session={makeSession("gemini-session", {
          agent: "Gemini",
          geminiApprovalMode: "default",
          modelOptions: [{ label: "Auto", value: "auto" }],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Refresh models" }));

    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(onRequestModelOptions).toHaveBeenCalledWith("gemini-session");
  });
});
