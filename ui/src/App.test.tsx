import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import App, {
  MarkdownContent,
  ThemedCombobox,
  describeCodexModelAdjustmentNotice,
  describeSessionModelRefreshError,
  describeUnknownSessionModelWarning,
} from "./App";
import type { AgentReadiness, Session } from "./types";

class EventSourceMock {
  addEventListener() {}

  removeEventListener() {}

  close() {}
}

class ResizeObserverMock {
  disconnect() {}

  observe() {}

  unobserve() {}
}

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    headers: {
      "Content-Type": "application/json",
    },
    status: 200,
  });
}

function makeSession(id: string, overrides?: Partial<Session>): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt-5.4",
    approvalPolicy: "never",
    reasoningEffort: "medium",
    sandboxMode: "workspace-write",
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

function makeReadiness(overrides?: Partial<AgentReadiness>): AgentReadiness {
  return {
    agent: "Gemini",
    status: "needsSetup",
    blocking: true,
    detail: "Gemini CLI needs auth before TermAl can create sessions.",
    commandPath: "/usr/local/bin/gemini",
    ...overrides,
  };
}

describe("MarkdownContent", () => {
  it("wraps markdown tables in a scroll container", () => {
    const markdown = [
      "| Finding | Resolution |",
      "| --- | --- |",
      "| `skip_list.rs` | Fixed |",
    ].join("\n");

    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    try {
      const { container } = render(<MarkdownContent markdown={markdown} />);

      const tableScroll = container.querySelector(".markdown-table-scroll");
      expect(tableScroll).not.toBeNull();
      expect(tableScroll?.querySelector("table")).not.toBeNull();
      expect(consoleError).not.toHaveBeenCalled();
    } finally {
      consoleError.mockRestore();
    }
  });
});

describe("App", () => {
  it("applies the active combobox option on space without closing the menu", () => {
    const onChange = vi.fn();
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      render(
        <ThemedCombobox
          id="test-combobox"
          value="gpt-5"
          options={[
            { label: "GPT-5", value: "gpt-5" },
            { label: "GPT-5 mini", value: "gpt-5-mini" },
          ]}
          onChange={onChange}
        />,
      );

      fireEvent.click(screen.getByRole("combobox"));
      fireEvent.keyDown(window, { key: "ArrowDown" });
      fireEvent.keyDown(window, { key: " " });

      expect(onChange).toHaveBeenCalledTimes(1);
      expect(onChange).toHaveBeenCalledWith("gpt-5-mini");
      expect(screen.getByRole("listbox")).toBeInTheDocument();

      fireEvent.keyDown(window, { key: "Enter" });

      expect(onChange).toHaveBeenCalledTimes(2);
      expect(onChange).toHaveBeenLastCalledWith("gpt-5-mini");
      expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
    }
  });

  it("describes when a Codex model switch resets reasoning effort", () => {
    expect(
      describeCodexModelAdjustmentNotice(
        makeSession("before", {
          model: "gpt-5",
          reasoningEffort: "minimal",
          modelOptions: [
            {
              label: "GPT-5",
              value: "gpt-5",
              supportedReasoningEfforts: ["minimal", "low", "medium", "high"],
            },
          ],
        }),
        makeSession("after", {
          model: "gpt-5-codex-mini",
          reasoningEffort: "medium",
          modelOptions: [
            {
              label: "GPT-5 Codex Mini",
              value: "gpt-5-codex-mini",
              supportedReasoningEfforts: ["medium", "high"],
            },
          ],
        }),
      ),
    ).toBe(
      "GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium.",
    );
  });

  it("rewrites model refresh failures into agent-specific guidance", () => {
    expect(
      describeSessionModelRefreshError(
        "Gemini",
        "failed to refresh Gemini model options: auth missing",
        makeReadiness(),
      ),
    ).toBe("Gemini CLI needs auth before TermAl can create sessions.");
    expect(
      describeSessionModelRefreshError(
        "Claude",
        "timed out refreshing Claude model options",
      ),
    ).toBe(
      "Claude did not return its live model list in time. Try Refresh models again. If this keeps happening, start a new Claude session.",
    );
  });

  it("warns before sending a prompt with an unknown session model", () => {
    expect(
      describeUnknownSessionModelWarning(
        makeSession("unknown-model", {
          agent: "Codex",
          model: "gpt-5.5-preview",
          modelOptions: [{ label: "GPT-5.4", value: "gpt-5.4" }],
        }),
      ),
    ).toBe(
      "Codex is set to gpt-5.5-preview, but that model is not in the current live list. Refresh models to verify it, or send the prompt again to continue anyway.",
    );
  });

  it("refreshes model options after creating a new Codex session", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (url === "/api/sessions") {
        return jsonResponse({
          sessionId: "session-1",
          state: {
            revision: 2,
            projects: [],
            sessions: [
              {
                id: "session-1",
                name: "Codex 1",
                emoji: "O",
                agent: "Codex",
                workdir: "/tmp",
                model: "gpt-5.4",
                approvalPolicy: "never",
                reasoningEffort: "medium",
                sandboxMode: "workspace-write",
                status: "idle",
                preview: "Ready for a prompt.",
                messages: [],
              },
            ],
          },
        });
      }
      if (url === "/api/sessions/session-1/model-options/refresh") {
        return jsonResponse({
          revision: 3,
          projects: [],
          sessions: [
            {
              id: "session-1",
              name: "Codex 1",
              emoji: "O",
              agent: "Codex",
              workdir: "/tmp",
              model: "gpt-5.4",
              modelOptions: [
                {
                  label: "gpt-5.4",
                  value: "gpt-5.4",
                  description: "Latest frontier agentic coding model.",
                  defaultReasoningEffort: "medium",
                  supportedReasoningEfforts: ["low", "medium", "high", "xhigh"],
                },
              ],
              approvalPolicy: "never",
              reasoningEffort: "medium",
              sandboxMode: "workspace-write",
              status: "idle",
              preview: "Ready for a prompt.",
              messages: [],
            },
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      render(<App />);

      fireEvent.click(await screen.findByRole("button", { name: "New Session" }));
      fireEvent.click(screen.getByRole("button", { name: "Create session" }));

      await waitFor(() => {
        expect(
          fetchMock.mock.calls.some(
            ([url]) => String(url) === "/api/sessions/session-1/model-options/refresh",
          ),
        ).toBe(true);
      });
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      vi.unstubAllGlobals();
    }
  });

  it("shows a Codex notice when live model refresh resets reasoning effort after session creation", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (url === "/api/sessions") {
        return jsonResponse({
          sessionId: "session-1",
          state: {
            revision: 2,
            projects: [],
            sessions: [
              {
                id: "session-1",
                name: "Codex 1",
                emoji: "O",
                agent: "Codex",
                workdir: "/tmp",
                model: "gpt-5-codex-mini",
                approvalPolicy: "never",
                reasoningEffort: "minimal",
                sandboxMode: "workspace-write",
                status: "idle",
                preview: "Ready for a prompt.",
                messages: [],
              },
            ],
          },
        });
      }
      if (url === "/api/sessions/session-1/model-options/refresh") {
        return jsonResponse({
          revision: 3,
          projects: [],
          sessions: [
            {
              id: "session-1",
              name: "Codex 1",
              emoji: "O",
              agent: "Codex",
              workdir: "/tmp",
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
              approvalPolicy: "never",
              reasoningEffort: "medium",
              sandboxMode: "workspace-write",
              status: "idle",
              preview: "Ready for a prompt.",
              messages: [],
            },
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      render(<App />);

      fireEvent.click(await screen.findByRole("button", { name: "New Session" }));
      fireEvent.click(screen.getByRole("button", { name: "Create session" }));
      fireEvent.click(await screen.findByRole("button", { name: "Prompt" }));

      await waitFor(() => {
        expect(
          screen.getByText(
            "GPT-5 Codex Mini only supports medium and high reasoning, so TermAl reset effort from minimal to medium.",
          ),
        ).toBeInTheDocument();
      });
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      vi.unstubAllGlobals();
    }
  });

  it("warns once before sending with an unknown model, then lets the second send continue", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/state") {
        return jsonResponse({
          revision: 1,
          projects: [],
          sessions: [],
        });
      }
      if (url === "/api/sessions") {
        return jsonResponse({
          sessionId: "session-1",
          state: {
            revision: 2,
            projects: [],
            sessions: [
              {
                id: "session-1",
                name: "Codex 1",
                emoji: "O",
                agent: "Codex",
                workdir: "/tmp",
                model: "gpt-5.5-preview",
                approvalPolicy: "never",
                reasoningEffort: "medium",
                sandboxMode: "workspace-write",
                status: "idle",
                preview: "Ready for a prompt.",
                messages: [],
              },
            ],
          },
        });
      }
      if (url === "/api/sessions/session-1/model-options/refresh") {
        return jsonResponse({
          revision: 3,
          projects: [],
          sessions: [
            {
              id: "session-1",
              name: "Codex 1",
              emoji: "O",
              agent: "Codex",
              workdir: "/tmp",
              model: "gpt-5.5-preview",
              modelOptions: [
                {
                  label: "GPT-5.4",
                  value: "gpt-5.4",
                  description: "Latest frontier agentic coding model.",
                },
              ],
              approvalPolicy: "never",
              reasoningEffort: "medium",
              sandboxMode: "workspace-write",
              status: "idle",
              preview: "Ready for a prompt.",
              messages: [],
            },
          ],
        });
      }
      if (url === "/api/sessions/session-1/messages") {
        return jsonResponse({
          revision: 4,
          projects: [],
          sessions: [
            {
              id: "session-1",
              name: "Codex 1",
              emoji: "O",
              agent: "Codex",
              workdir: "/tmp",
              model: "gpt-5.5-preview",
              modelOptions: [
                {
                  label: "GPT-5.4",
                  value: "gpt-5.4",
                  description: "Latest frontier agentic coding model.",
                },
              ],
              approvalPolicy: "never",
              reasoningEffort: "medium",
              sandboxMode: "workspace-write",
              status: "active",
              preview: "Working...",
              messages: [],
            },
          ],
        });
      }

      throw new Error(`Unexpected fetch: ${url}`);
    });

    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = vi.fn();

    try {
      render(<App />);

      fireEvent.click(await screen.findByRole("button", { name: "New Session" }));
      fireEvent.click(screen.getByRole("button", { name: "Create session" }));

      const textarea = await screen.findByLabelText("Message Codex 1");
      fireEvent.change(textarea, { target: { value: "Investigate this session." } });
      fireEvent.click(screen.getByRole("button", { name: "Send" }));

      expect(
        screen.getByText(
          "Codex is set to gpt-5.5-preview, but that model is not in the current live list. Refresh models to verify it, or send the prompt again to continue anyway.",
        ),
      ).toBeInTheDocument();
      expect(
        fetchMock.mock.calls.some(([url]) => String(url) === "/api/sessions/session-1/messages"),
      ).toBe(false);
      expect(screen.getByLabelText("Message Codex 1")).toHaveValue("Investigate this session.");

      fireEvent.click(screen.getByRole("button", { name: "Send" }));

      await waitFor(() => {
        expect(
          fetchMock.mock.calls.some(([url]) => String(url) === "/api/sessions/session-1/messages"),
        ).toBe(true);
      });
    } finally {
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
      vi.unstubAllGlobals();
    }
  });
});
