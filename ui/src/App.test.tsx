import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import App, { MarkdownContent } from "./App";

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
});
