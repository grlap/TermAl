import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  ClaudePromptSettingsCard,
  CodexPromptSettingsCard,
  CursorPromptSettingsCard,
  GeminiPromptSettingsCard,
  OpenCodePromptSettingsCard,
} from "./prompt-settings-cards";
import {
  SESSION_MODEL_OPTIONS_DEFERRED_RETRY_LIMIT,
  sessionModelOptionsDeferredRetryDelay,
} from "./session-model-refresh-retry";
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

const noopArchiveThread = () => {};
const noopCompactThread = () => {};
const noopForkThread = () => {};
const noopRollbackThread = () => {};
const noopUnarchiveThread = () => {};

describe("session model refresh controls", () => {
  it("auto-requests Codex model options when the session card opens without a live list", async () => {
    const onRequestModelOptions = vi.fn();

    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={onRequestModelOptions}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    await waitFor(() => {
      expect(onRequestModelOptions).toHaveBeenCalledWith("codex-session");
    });
    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("button", { name: "Refresh models" }),
    ).toBeInTheDocument();
  });

  it("defers automatic model refresh until an active session returns idle", async () => {
    const onRequestModelOptions = vi.fn();
    const activeSession = makeSession("codex-active", {
      agent: "Codex",
      approvalPolicy: "never",
      reasoningEffort: "medium",
      sandboxMode: "workspace-write",
      model: "gpt-5.4",
      status: "active",
    });
    const { rerender } = render(
      <CodexPromptSettingsCard
        paneId="pane-codex-active"
        session={activeSession}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={onRequestModelOptions}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    expect(onRequestModelOptions).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Refresh models" })).toBeDisabled();

    rerender(
      <CodexPromptSettingsCard
        paneId="pane-codex-active"
        session={{ ...activeSession, status: "idle" }}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={onRequestModelOptions}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    await waitFor(() => {
      expect(onRequestModelOptions).toHaveBeenCalledWith("codex-active");
    });
    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Refresh models" })).toBeEnabled();
  });

  it("waits for pending Engram revocation and rearms after that lifecycle fence clears", async () => {
    vi.useFakeTimers();
    try {
      const onRequestModelOptions = vi
        .fn()
        .mockResolvedValueOnce("deferred")
        .mockResolvedValueOnce("refreshed");
      const idleSession = makeSession("codex-pending-revocation", {
        agent: "Codex",
        approvalPolicy: "never",
        reasoningEffort: "medium",
        sandboxMode: "workspace-write",
        model: "gpt-5.4",
      });
      const renderCard = (isEngramMcpRevocationPending: boolean) => (
        <CodexPromptSettingsCard
          paneId="pane-codex-pending-revocation"
          session={idleSession}
          isUpdating={false}
          isRefreshingModelOptions={false}
          isEngramMcpRevocationPending={isEngramMcpRevocationPending}
          modelOptionsError={null}
          sessionNotice={null}
          onArchiveThread={noopArchiveThread}
          onCompactThread={noopCompactThread}
          onForkThread={noopForkThread}
          onRequestModelOptions={onRequestModelOptions}
          onRollbackThread={noopRollbackThread}
          onSessionSettingsChange={() => {}}
          onUnarchiveThread={noopUnarchiveThread}
        />
      );
      const { rerender } = render(renderCard(false));
      await act(async () => {
        await Promise.resolve();
      });
      expect(onRequestModelOptions).toHaveBeenCalledTimes(1);

      rerender(renderCard(true));
      expect(screen.getByRole("button", { name: "Refresh models" })).toBeDisabled();
      await act(async () => {
        vi.runAllTimers();
        await Promise.resolve();
      });
      expect(onRequestModelOptions).toHaveBeenCalledTimes(1);

      rerender(renderCard(false));
      await act(async () => {
        await Promise.resolve();
      });
      expect(onRequestModelOptions).toHaveBeenCalledTimes(2);
      expect(screen.getByRole("button", { name: "Refresh models" })).toBeEnabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("retries an idle lifecycle conflict without rearming ordinary failures after every turn", async () => {
    vi.useFakeTimers();
    try {
      const onRequestModelOptions = vi
        .fn()
        .mockResolvedValueOnce("deferred")
        .mockResolvedValue("failed");
      const idleSession = makeSession("codex-retry", {
        agent: "Codex",
        approvalPolicy: "never",
        reasoningEffort: "medium",
        sandboxMode: "workspace-write",
        model: "gpt-5.4",
      });
      const renderCard = (session: Session) => (
        <CodexPromptSettingsCard
          paneId="pane-codex-retry"
          session={session}
          isUpdating={false}
          isRefreshingModelOptions={false}
          modelOptionsError={null}
          sessionNotice={null}
          onArchiveThread={noopArchiveThread}
          onCompactThread={noopCompactThread}
          onForkThread={noopForkThread}
          onRequestModelOptions={onRequestModelOptions}
          onRollbackThread={noopRollbackThread}
          onSessionSettingsChange={() => {}}
          onUnarchiveThread={noopUnarchiveThread}
        />
      );
      const { rerender } = render(renderCard(idleSession));

      await act(async () => {
        await Promise.resolve();
      });
      expect(onRequestModelOptions).toHaveBeenCalledTimes(1);

      await act(async () => {
        vi.advanceTimersByTime(500);
        await Promise.resolve();
      });
      expect(onRequestModelOptions).toHaveBeenCalledTimes(2);

      rerender(renderCard({ ...idleSession, status: "active" }));
      rerender(renderCard(idleSession));
      rerender(renderCard({ ...idleSession, status: "active" }));
      rerender(renderCard(idleSession));
      await act(async () => {
        await Promise.resolve();
      });

      expect(onRequestModelOptions).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not schedule a deferred retry after the settings card unmounts", async () => {
    vi.useFakeTimers();
    try {
      let resolveRefresh: ((outcome: "deferred") => void) | null = null;
      const onRequestModelOptions = vi.fn(
        () =>
          new Promise<"deferred">((resolve) => {
            resolveRefresh = resolve;
          }),
      );
      const { unmount } = render(
        <CodexPromptSettingsCard
          paneId="pane-codex-unmount"
          session={makeSession("codex-unmount", {
            agent: "Codex",
            approvalPolicy: "never",
            reasoningEffort: "medium",
            sandboxMode: "workspace-write",
            model: "gpt-5.4",
          })}
          isUpdating={false}
          isRefreshingModelOptions={false}
          modelOptionsError={null}
          sessionNotice={null}
          onArchiveThread={noopArchiveThread}
          onCompactThread={noopCompactThread}
          onForkThread={noopForkThread}
          onRequestModelOptions={onRequestModelOptions}
          onRollbackThread={noopRollbackThread}
          onSessionSettingsChange={() => {}}
          onUnarchiveThread={noopUnarchiveThread}
        />,
      );
      await act(async () => {
        await Promise.resolve();
      });
      expect(onRequestModelOptions).toHaveBeenCalledTimes(1);

      unmount();
      await act(async () => {
        resolveRefresh?.("deferred");
        await Promise.resolve();
      });
      expect(vi.getTimerCount()).toBe(0);
      await act(async () => {
        vi.runAllTimers();
      });
      expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("stays exhausted after the bounded deferred retry sequence rerenders", async () => {
    vi.useFakeTimers();
    try {
      const onRequestModelOptions = vi.fn().mockResolvedValue("deferred");
      const idleSession = makeSession("codex-exhausted", {
        agent: "Codex",
        approvalPolicy: "never",
        reasoningEffort: "medium",
        sandboxMode: "workspace-write",
        model: "gpt-5.4",
      });
      const renderCard = (isRefreshingModelOptions: boolean) => (
        <CodexPromptSettingsCard
          paneId="pane-codex-exhausted"
          session={idleSession}
          isUpdating={false}
          isRefreshingModelOptions={isRefreshingModelOptions}
          modelOptionsError={null}
          sessionNotice={null}
          onArchiveThread={noopArchiveThread}
          onCompactThread={noopCompactThread}
          onForkThread={noopForkThread}
          onRequestModelOptions={onRequestModelOptions}
          onRollbackThread={noopRollbackThread}
          onSessionSettingsChange={() => {}}
          onUnarchiveThread={noopUnarchiveThread}
        />
      );
      const { rerender } = render(renderCard(false));

      await act(async () => {
        await Promise.resolve();
      });
      for (
        let attempt = 0;
        attempt < SESSION_MODEL_OPTIONS_DEFERRED_RETRY_LIMIT;
        attempt += 1
      ) {
        await act(async () => {
          vi.advanceTimersByTime(
            sessionModelOptionsDeferredRetryDelay(attempt),
          );
          await Promise.resolve();
        });
      }
      expect(onRequestModelOptions).toHaveBeenCalledTimes(
        SESSION_MODEL_OPTIONS_DEFERRED_RETRY_LIMIT,
      );

      // Production request state toggles around every HTTP call. Exhaustion
      // must remain a latch when the final true -> false transition rerenders.
      rerender(renderCard(true));
      rerender(renderCard(false));
      await act(async () => {
        await Promise.resolve();
      });
      expect(onRequestModelOptions).toHaveBeenCalledTimes(
        SESSION_MODEL_OPTIONS_DEFERRED_RETRY_LIMIT,
      );
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("limits Codex reasoning effort choices to the selected model capabilities", () => {
    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "high",
          sandboxMode: "workspace-write",
          model: "gpt-5-codex-mini",
          modelOptions: [
            {
              label: "GPT-5 Codex Mini",
              value: "gpt-5-codex-mini",
              description: "Optimized for codex. Cheaper, faster, but less capable.",
              defaultReasoningEffort: "medium",
              supportedReasoningEfforts: ["medium", "high"],
            },
          ],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice="GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium."
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    expect(
      screen.getByText((content) =>
        content.includes("This model supports medium, high reasoning. medium is the default."),
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium.",
      ),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("Reasoning effort"));

    expect(screen.getByRole("option", { name: /medium/i })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /high/i })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /minimal/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /^low/i })).not.toBeInTheDocument();
  });

  it("shows Fast mode only when the active Codex model advertises it", () => {
    const onSessionSettingsChange = vi.fn();
    render(
      <CodexPromptSettingsCard
        paneId="pane-fast"
        session={makeSession("codex-fast", {
          agent: "Codex",
          model: "gpt-5.5",
          modelOptions: [
            {
              label: "GPT-5.5",
              value: "gpt-5.5",
              serviceTiers: [
                {
                  id: "priority",
                  label: "Fast",
                  description: "1.5x speed, increased usage",
                },
              ],
            },
          ],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={onSessionSettingsChange}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    fireEvent.click(screen.getByLabelText("Response speed"));
    fireEvent.click(screen.getByRole("option", { name: /Fast/i }));
    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "codex-fast",
      "codexFastMode",
      "on",
    );
  });

  it("keeps the Standard control available for persisted Fast mode before catalog refresh", () => {
    const onSessionSettingsChange = vi.fn();
    render(
      <CodexPromptSettingsCard
        paneId="pane-fast-loading"
        session={makeSession("codex-fast-loading", {
          agent: "Codex",
          codexFastMode: true,
          model: "gpt-5.5",
          modelOptions: undefined,
        })}
        isUpdating={false}
        isRefreshingModelOptions={true}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={onSessionSettingsChange}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    fireEvent.click(screen.getByLabelText("Response speed"));
    fireEvent.click(screen.getByRole("option", { name: /Standard/i }));
    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "codex-fast-loading",
      "codexFastMode",
      "off",
    );
  });

  it("auto-requests Claude model options when the session card opens without a live list", async () => {
    const onRequestModelOptions = vi.fn();

    render(
      <ClaudePromptSettingsCard
        paneId="pane-claude"
        session={makeSession("claude-session", {
          agent: "Claude",
          claudeApprovalMode: "ask",
          model: "sonnet",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    await waitFor(() => {
      expect(onRequestModelOptions).toHaveBeenCalledWith("claude-session");
    });
    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("button", { name: "Refresh models" }),
    ).toBeInTheDocument();
  });

  it("canonicalizes a known manual Claude model label from the session card", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      <ClaudePromptSettingsCard
        paneId="pane-claude"
        session={makeSession("claude-session", {
          agent: "Claude",
          claudeApprovalMode: "ask",
          model: "sonnet",
          modelOptions: [
            {
              label: "Default (recommended)",
              value: "default",
              description: "Opus 4.6 · Most capable for complex work",
              badges: ["Recommended", "Effort", "Adaptive", "Fast"],
            },
            {
              label: "Sonnet",
              value: "sonnet",
              description: "Sonnet 4.6 · Best for everyday tasks",
              badges: ["Effort"],
            },
          ],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        onRequestModelOptions={() => {}}
        onSessionSettingsChange={onSessionSettingsChange}
      />,
    );

    fireEvent.change(screen.getByLabelText("Manual model id"), {
      target: { value: "Default (recommended)" },
    });
    expect(
      screen.getByText(
        "Matches Default (recommended) from the current live list. TermAl will apply default.",
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "claude-session",
      "model",
      "default",
    );
    expect(screen.getByText("Sonnet 4.6 · Best for everyday tasks")).toBeInTheDocument();
    expect(screen.getByText("Effort")).toBeInTheDocument();
  });

  it("lets Codex apply a manual model id from the session card", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
          modelOptions: [{ label: "GPT-5.4", value: "gpt-5.4" }],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={onSessionSettingsChange}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    fireEvent.change(screen.getByLabelText("Manual model id"), {
      target: { value: "gpt-5.5-preview" },
    });
    expect(
      screen.getByText(
        "gpt-5.5-preview is not in the current live model list. TermAl will still try it on the next prompt.",
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "codex-session",
      "model",
      "gpt-5.5-preview",
    );
  });

  it("disables Codex thread actions until the session has a live thread id", () => {
    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    expect(screen.getByRole("button", { name: "Fork thread" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Compact" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Archive" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Unarchive" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Roll back" })).toBeDisabled();
  });

  it("fires Codex thread actions from the session card when a live thread exists", () => {
    const onArchiveThread = vi.fn();
    const onCompactThread = vi.fn();
    const onForkThread = vi.fn();
    const onRollbackThread = vi.fn();
    const onUnarchiveThread = vi.fn();

    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
          externalSessionId: "thread-live",
          codexThreadState: "active",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={onArchiveThread}
        onCompactThread={onCompactThread}
        onForkThread={onForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={onRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={onUnarchiveThread}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Fork thread" }));
    fireEvent.click(screen.getByRole("button", { name: "Compact" }));
    fireEvent.click(screen.getByRole("button", { name: "Archive" }));
    expect(screen.getByRole("button", { name: "Unarchive" })).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Roll back turns"), {
      target: { value: "3" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Roll back" }));

    expect(onForkThread).toHaveBeenCalledWith("codex-session", "pane-codex");
    expect(onCompactThread).toHaveBeenCalledWith("codex-session");
    expect(onArchiveThread).toHaveBeenCalledWith("codex-session");
    expect(onUnarchiveThread).not.toHaveBeenCalled();
    expect(onRollbackThread).toHaveBeenCalledWith("codex-session", 3);
  });

  it("disables Codex thread actions while prompts are queued", () => {
    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
          externalSessionId: "thread-live",
          codexThreadState: "active",
          pendingPrompts: [
            {
              id: "pending-1",
              timestamp: "2026-03-20T10:00:00Z",
              text: "finish the queued review",
            },
          ],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={noopArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={noopUnarchiveThread}
      />,
    );

    expect(screen.getByText("Wait for queued Codex prompts to finish before changing the live thread.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Fork thread" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Compact" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Archive" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Unarchive" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Roll back" })).toBeDisabled();
  });

  it("switches the Codex archive controls when the live thread is archived", () => {
    const onArchiveThread = vi.fn();
    const onUnarchiveThread = vi.fn();

    render(
      <CodexPromptSettingsCard
        paneId="pane-codex"
        session={makeSession("codex-session", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
          externalSessionId: "thread-live",
          codexThreadState: "archived",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        sessionNotice={null}
        onArchiveThread={onArchiveThread}
        onCompactThread={noopCompactThread}
        onForkThread={noopForkThread}
        onRequestModelOptions={() => {}}
        onRollbackThread={noopRollbackThread}
        onSessionSettingsChange={() => {}}
        onUnarchiveThread={onUnarchiveThread}
      />,
    );

    expect(screen.getByText("This Codex thread is archived. Unarchive it before sending another prompt.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Archive" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Unarchive" }));

    expect(onArchiveThread).not.toHaveBeenCalled();
    expect(onUnarchiveThread).toHaveBeenCalledWith("codex-session");
  });

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
        modelOptionsError={null}
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
        modelOptionsError={null}
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Refresh models" }));

    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
    expect(onRequestModelOptions).toHaveBeenCalledWith("gemini-session");
  });

  it("shows inline refresh errors in the session card", () => {
    render(
      <GeminiPromptSettingsCard
        paneId="pane-gemini"
        session={makeSession("gemini-session", {
          agent: "Gemini",
          geminiApprovalMode: "default",
          model: "auto",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError="Gemini CLI is not authenticated."
        onRequestModelOptions={() => {}}
        onSessionSettingsChange={() => {}}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Could not refresh Gemini's live model list for this session. Gemini CLI is not authenticated.",
    );
  });

  it("renders OpenCode's live model, reasoning-variant, and mode choices with explicit Auto authority", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      <OpenCodePromptSettingsCard
        paneId="pane-opencode"
        session={makeSession("opencode-session", {
          agent: "OpenCode",
          model: "opencode/big-pickle",
          opencodeModel: "auto",
          opencodeEffort: "auto",
          opencodeCurrentEffort: "medium",
          opencodeMode: "auto",
          opencodeCurrentMode: "build",
          modelOptions: [
            { label: "Big Pickle", value: "opencode/big-pickle" },
            { label: "GPT-5.6 Sol", value: "openai/gpt-5.6-sol" },
          ],
          opencodeModeOptions: [
            { label: "Build", value: "build" },
            { label: "Plan", value: "plan" },
          ],
          opencodeEffortOptions: [
            { label: "Low", value: "low" },
            { label: "High", value: "high" },
          ],
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        onRequestModelOptions={() => {}}
        onSessionSettingsChange={onSessionSettingsChange}
      />,
    );

    expect(screen.getByText(/Effective model:/u)).toHaveTextContent(
      "Effective model: opencode/big-pickle",
    );
    expect(screen.getByText(/Effective mode:/u)).toHaveTextContent(
      "Effective mode: build",
    );
    expect(screen.getByText(/Effective variant:/u)).toHaveTextContent(
      "Effective variant: medium",
    );

    fireEvent.click(screen.getByRole("combobox", { name: "OpenCode model" }));
    expect(screen.getByRole("option", { name: /Auto/u })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("option", { name: /GPT-5.6 Sol/u }));
    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "opencode-session",
      "model",
      "openai/gpt-5.6-sol",
    );

    fireEvent.click(
      screen.getByRole("combobox", { name: "OpenCode reasoning variant" }),
    );
    fireEvent.click(screen.getByRole("option", { name: /High/u }));
    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "opencode-session",
      "opencodeEffort",
      "high",
    );

    fireEvent.click(screen.getByRole("combobox", { name: "OpenCode mode" }));
    fireEvent.click(screen.getByRole("option", { name: /Plan/u }));
    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "opencode-session",
      "opencodeMode",
      "plan",
    );
  });

  it("auto-requests OpenCode config options when the session card has no live list", async () => {
    const onRequestModelOptions = vi.fn();

    render(
      <OpenCodePromptSettingsCard
        paneId="pane-opencode"
        session={makeSession("opencode-session", {
          agent: "OpenCode",
          model: "auto",
          opencodeModel: "auto",
          opencodeMode: "auto",
        })}
        isUpdating={false}
        isRefreshingModelOptions={false}
        modelOptionsError={null}
        onRequestModelOptions={onRequestModelOptions}
        onSessionSettingsChange={() => {}}
      />,
    );

    await waitFor(() => {
      expect(onRequestModelOptions).toHaveBeenCalledWith("opencode-session");
    });
    expect(onRequestModelOptions).toHaveBeenCalledTimes(1);
  });
});
