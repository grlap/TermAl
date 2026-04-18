import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import mermaid from "mermaid";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { MarkdownContent, MessageCard, areMarkdownLineMarkersEqual } from "./message-cards";
import type {
  CodexAppRequestMessage,
  McpElicitationRequestMessage,
  UserInputRequestMessage,
} from "./types";

vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    render: vi.fn(async (id: string) => ({
      diagramType: "flowchart",
      svg: `<svg data-testid="mermaid-svg" id="${id}"><text>diagram</text></svg>`,
    })),
  },
}));

const mermaidInitializeMock = vi.mocked(mermaid.initialize);
const mermaidRenderMock = vi.mocked(mermaid.render);

beforeEach(() => {
  mermaidInitializeMock.mockClear();
  mermaidRenderMock.mockClear();
  mermaidRenderMock.mockResolvedValue({
    diagramType: "flowchart",
    svg: '<svg data-testid="mermaid-svg"><text>diagram</text></svg>',
  });
});

describe("MarkdownContent inline file links", () => {
  it("renders inline code file references as clickable links when the source callback exists", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="Text like `experience.tex.#L63` should stay clickable."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const link = screen.getByRole("link", { name: "experience.tex.#L63" });
    expect(link).toHaveClass("inline-code-link");
    expect(link).toHaveAttribute("draggable", "false");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("renders inline code file references as plain code without the source callback", () => {
    render(
      <MarkdownContent
        markdown="Text like `experience.tex.#L63` should stay plain code."
        workspaceRoot="/repo"
      />,
    );

    expect(
      screen.queryByRole("link", { name: "experience.tex.#L63" }),
    ).toBeNull();
    expect(
      screen.getByText("experience.tex.#L63", { selector: "code" }).closest("a"),
    ).toBeNull();
  });

  it("preserves inline code link DOM nodes when the source callback identity changes", () => {
    const firstOnOpenSourceLink = vi.fn();
    const secondOnOpenSourceLink = vi.fn();
    const markdown = "Text like `experience.tex.#L63` should stay clickable.";
    const { rerender } = render(
      <MarkdownContent
        markdown={markdown}
        onOpenSourceLink={firstOnOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const firstLink = screen.getByRole("link", { name: "experience.tex.#L63" });
    const firstCode = firstLink.querySelector("code");

    rerender(
      <MarkdownContent
        markdown={markdown}
        onOpenSourceLink={secondOnOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const secondLink = screen.getByRole("link", { name: "experience.tex.#L63" });
    expect(secondLink).toBe(firstLink);
    expect(secondLink.querySelector("code")).toBe(firstCode);

    fireEvent.click(secondLink);

    expect(firstOnOpenSourceLink).not.toHaveBeenCalled();
    expect(secondOnOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("renders Markdown images as non-draggable", () => {
    render(<MarkdownContent markdown="![Diagram](./diagram.png)" workspaceRoot="/repo" />);

    expect(screen.getByRole("img", { name: "Diagram" })).toHaveAttribute("draggable", "false");
  });
});

describe("MarkdownContent Mermaid diagrams", () => {
  it("rerenders memoized message cards when appearance changes", async () => {
    const message = {
      author: "assistant",
      id: "message-mermaid-theme",
      text: ["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n"),
      timestamp: "2026-04-15T00:00:00.000Z",
      type: "text",
    } as const;
    const noop = () => {};
    const { rerender } = render(
      <MessageCard
        appearance="dark"
        message={message}
        onApprovalDecision={noop}
        onUserInputSubmit={noop}
        preferImmediateHeavyRender
      />,
    );

    await waitFor(() => {
      expect(
        mermaidInitializeMock.mock.calls.some(([config]) => config.theme === "dark"),
      ).toBe(true);
    });
    const callCountAfterDarkRender = mermaidInitializeMock.mock.calls.length;

    rerender(
      <MessageCard
        appearance="light"
        message={message}
        onApprovalDecision={noop}
        onUserInputSubmit={noop}
        preferImmediateHeavyRender
      />,
    );

    await waitFor(() => {
      expect(
        mermaidInitializeMock.mock.calls
          .slice(callCountAfterDarkRender)
          .some(([config]) => config.theme === "default"),
      ).toBe(true);
    });
  });

  it("renders Mermaid fenced blocks as diagrams", async () => {
    render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    expect(await screen.findByTestId("mermaid-frame")).toBeInTheDocument();
    expect(mermaidInitializeMock).toHaveBeenNthCalledWith(1, {
      darkMode: true,
      flowchart: {
        defaultRenderer: "dagre-wrapper",
        diagramPadding: 1,
        nodeSpacing: 12,
        padding: 3,
        rankSpacing: 18,
        useMaxWidth: false,
        wrappingWidth: 90,
      },
      htmlLabels: true,
      securityLevel: "strict",
      startOnLoad: false,
      theme: "dark",
      themeCSS: expect.stringContaining("border-radius: 12px"),
      themeVariables: { fontSize: "11px" },
    });
    expect(mermaidInitializeMock).toHaveBeenNthCalledWith(2, {
      flowchart: {
        defaultRenderer: "dagre-wrapper",
        diagramPadding: 1,
        nodeSpacing: 12,
        padding: 3,
        rankSpacing: 18,
        useMaxWidth: false,
        wrappingWidth: 90,
      },
      htmlLabels: true,
      securityLevel: "strict",
      startOnLoad: false,
      themeCSS: expect.stringContaining("border-radius: 12px"),
      themeVariables: { fontSize: "11px" },
    });
    expect(mermaidRenderMock).toHaveBeenCalledWith(
      expect.stringMatching(/^termal-mermaid-\d+$/),
      "flowchart TD\n  A --> B",
    );
  });

  it("uses light Mermaid colors when Markdown appears in a light editor", async () => {
    render(
      <MarkdownContent
        appearance="light"
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    expect(await screen.findByTestId("mermaid-frame")).toBeInTheDocument();
    expect(mermaidInitializeMock).toHaveBeenNthCalledWith(1, expect.objectContaining({
      darkMode: false,
      theme: "default",
    }));
  });

  it("renders Mermaid source without rewriting labels", async () => {
    render(
      <MarkdownContent
        markdown={[
          "```mermaid",
          "flowchart TD",
          "  Start --> Detect{Contains Mermaid fence?}",
          "```",
        ].join("\n")}
      />,
    );

    expect(await screen.findByTestId("mermaid-frame")).toBeInTheDocument();
    expect(mermaidRenderMock).toHaveBeenCalledWith(
      expect.stringMatching(/^termal-mermaid-\d+$/),
      "flowchart TD\n  Start --> Detect{Contains Mermaid fence?}",
    );
  });

  it("isolates Mermaid SVG output in a sandboxed frame", async () => {
    mermaidRenderMock.mockResolvedValueOnce({
      diagramType: "flowchart",
      svg: '<svg data-testid="mermaid-svg" onload="alert(1)"><script>alert(2)</script><text>diagram</text></svg>',
    });

    render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    const frame = await screen.findByTestId("mermaid-frame");
    expect(screen.queryByTestId("mermaid-svg")).not.toBeInTheDocument();
    expect(frame).toHaveAttribute("sandbox", "");
    expect(frame).toHaveAttribute("srcdoc", expect.stringContaining("onload"));
  });

  it("keeps Mermaid fenced blocks as source when diagram rendering is disabled", () => {
    const { container } = render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
        renderMermaidDiagrams={false}
      />,
    );

    expect(container.querySelector(".mermaid-diagram-block")).toBeNull();
    expect(container.querySelector("code.language-mermaid")?.textContent).toBe(
      "flowchart TD\n  A --> B",
    );
    expect(mermaidRenderMock).not.toHaveBeenCalled();
  });

  it("skips Mermaid rendering when a diagram exceeds the source budget", () => {
    const oversizedSource = `flowchart TD\n  A --> ${"B".repeat(50_001)}`;

    const { container } = render(
      <MarkdownContent
        markdown={["```mermaid", oversizedSource, "```"].join("\n")}
      />,
    );

    expect(
      screen.getByText("Mermaid render skipped: diagram exceeds the 50,000 character render budget."),
    ).toBeInTheDocument();
    expect(container.querySelector("code.language-mermaid")?.textContent).toBe(oversizedSource);
    expect(mermaidRenderMock).not.toHaveBeenCalled();
  });

  it("skips Mermaid rendering when a document has too many diagrams", () => {
    const markdown = Array.from({ length: 21 }, (_value, index) =>
      ["```mermaid", "flowchart TD", `  A${index} --> B${index}`, "```"].join("\n"),
    ).join("\n\n");

    render(<MarkdownContent markdown={markdown} />);

    expect(
      screen.getAllByText("Mermaid render skipped: document has 21 diagrams; the render budget is 20."),
    ).toHaveLength(21);
    expect(mermaidRenderMock).not.toHaveBeenCalled();
  });

  it("falls back to the Mermaid source when rendering fails", async () => {
    mermaidRenderMock.mockRejectedValueOnce(new Error("syntax error"));

    const { container } = render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A -->", "```"].join("\n")}
      />,
    );

    expect(await screen.findByText("Mermaid render failed: syntax error")).toBeInTheDocument();
    expect(container.querySelector("code.language-mermaid")?.textContent).toBe("flowchart TD\n  A -->");
  });

  it("caps Mermaid iframe dimensions when the SVG viewBox is pathologically large", async () => {
    // A huge `viewBox` — could be hostile Markdown, agent output with a very
    // large flowchart, or a bug in the renderer. The iframe is sandboxed so
    // this is a layout-DoS concern, not an XSS one. The rendered frame must
    // stay bounded so it cannot overflow its parent column.
    mermaidRenderMock.mockResolvedValueOnce({
      diagramType: "flowchart",
      svg: '<svg data-testid="mermaid-svg" viewBox="0 0 99999 99999"><text>huge</text></svg>',
    });

    const { container } = render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    const frame = await screen.findByTestId("mermaid-frame");
    const widthPx = Number.parseInt(frame.style.width, 10);
    const heightPx = Number.parseInt(frame.style.height, 10);
    expect(widthPx).toBeGreaterThan(0);
    expect(heightPx).toBeGreaterThan(0);
    expect(widthPx).toBeLessThanOrEqual(4096);
    expect(heightPx).toBeLessThanOrEqual(4096);
    expect(frame.style.maxWidth).toBe("100%");
    // The inline width/height cap is the primary guard; also confirm the
    // container did not propagate the runaway SVG dimensions.
    expect(container.querySelector(".mermaid-diagram-frame")).toBe(frame);
  });
});

describe("MessageCard memoization", () => {
  it("uses the latest approval handler after handler-only rerenders", () => {
    const firstHandler = vi.fn();
    const secondHandler = vi.fn();
    const noop = () => {};
    const message = {
      author: "assistant",
      command: "git status",
      decision: "pending",
      detail: "Approve this command.",
      id: "approval-1",
      timestamp: "2026-04-15T00:00:00.000Z",
      title: "Run command",
      type: "approval",
    } as const;
    const { rerender } = render(
      <MessageCard
        message={message}
        onApprovalDecision={firstHandler}
        onUserInputSubmit={noop}
      />,
    );

    rerender(
      <MessageCard
        message={message}
        onApprovalDecision={secondHandler}
        onUserInputSubmit={noop}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));

    expect(firstHandler).not.toHaveBeenCalled();
    expect(secondHandler).toHaveBeenCalledWith("approval-1", "accepted");
  });

  it("uses the latest user-input handler after handler-only rerenders", () => {
    const firstHandler = vi.fn();
    const secondHandler = vi.fn();
    const noop = () => {};
    const message: UserInputRequestMessage = {
      author: "assistant",
      detail: "Codex requested additional input.",
      id: "user-input-1",
      questions: [
        {
          header: "Ticket",
          id: "ticket",
          question: "Which ticket should I use?",
        },
      ],
      state: "pending",
      timestamp: "2026-04-15T00:00:00.000Z",
      title: "Codex needs input",
      type: "userInputRequest",
    };
    const { rerender } = render(
      <MessageCard
        message={message}
        onApprovalDecision={noop}
        onUserInputSubmit={firstHandler}
      />,
    );

    rerender(
      <MessageCard
        message={message}
        onApprovalDecision={noop}
        onUserInputSubmit={secondHandler}
      />,
    );

    fireEvent.change(screen.getByRole("textbox"), { target: { value: "TERM-42" } });
    fireEvent.click(screen.getByRole("button", { name: "Submit answers" }));

    expect(firstHandler).not.toHaveBeenCalled();
    expect(secondHandler).toHaveBeenCalledWith("user-input-1", {
      ticket: ["TERM-42"],
    });
  });

  it("uses the latest MCP elicitation handler after handler-only rerenders", () => {
    const firstHandler = vi.fn();
    const secondHandler = vi.fn();
    const noop = () => {};
    const message: McpElicitationRequestMessage = {
      author: "assistant",
      detail: "deployment-helper requested structured input.",
      id: "mcp-elicitation-1",
      request: {
        message: "Confirm the deployment settings.",
        mode: "form",
        requestedSchema: {
          properties: {
            environment: {
              oneOf: [
                { const: "production", title: "Production" },
                { const: "staging", title: "Staging" },
              ],
              title: "Environment",
              type: "string",
            },
          },
          required: ["environment"],
          type: "object",
        },
        serverName: "deployment-helper",
        threadId: "thread-1",
        turnId: "turn-1",
      },
      state: "pending",
      timestamp: "2026-04-15T00:00:00.000Z",
      title: "Codex needs MCP input",
      type: "mcpElicitationRequest",
    };
    const { rerender } = render(
      <MessageCard
        message={message}
        onApprovalDecision={noop}
        onMcpElicitationSubmit={firstHandler}
        onUserInputSubmit={noop}
      />,
    );

    rerender(
      <MessageCard
        message={message}
        onApprovalDecision={noop}
        onMcpElicitationSubmit={secondHandler}
        onUserInputSubmit={noop}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: "Production" }));
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    expect(firstHandler).not.toHaveBeenCalled();
    expect(secondHandler).toHaveBeenCalledWith("mcp-elicitation-1", "accept", {
      environment: "production",
    });
  });

  it("uses the latest Codex app-request handler after handler-only rerenders", () => {
    const firstHandler = vi.fn();
    const secondHandler = vi.fn();
    const noop = () => {};
    const message: CodexAppRequestMessage = {
      author: "assistant",
      detail: "Codex requested a result for `search_workspace`.",
      id: "codex-request-1",
      method: "item/tool/call",
      params: {
        arguments: {
          pattern: "Codex",
        },
        toolName: "search_workspace",
      },
      state: "pending",
      timestamp: "2026-04-15T00:00:00.000Z",
      title: "Codex needs a tool result",
      type: "codexAppRequest",
    };
    const { rerender } = render(
      <MessageCard
        message={message}
        onApprovalDecision={noop}
        onCodexAppRequestSubmit={firstHandler}
        onUserInputSubmit={noop}
      />,
    );

    rerender(
      <MessageCard
        message={message}
        onApprovalDecision={noop}
        onCodexAppRequestSubmit={secondHandler}
        onUserInputSubmit={noop}
      />,
    );

    fireEvent.change(screen.getByRole("textbox"), {
      target: { value: '{\n  "matches": ["docs/bugs.md"]\n}' },
    });
    fireEvent.click(screen.getByRole("button", { name: "Submit JSON result" }));

    expect(firstHandler).not.toHaveBeenCalled();
    expect(secondHandler).toHaveBeenCalledWith("codex-request-1", {
      matches: ["docs/bugs.md"],
    });
  });
});

describe("MarkdownContent document links", () => {
  it("resolves document-relative Markdown links with anchors and Windows roots", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        documentPath="C:/repo/docs/features/intro.md"
        markdown={[
          "[Sibling](./guide.md#L10)",
          "[Parent](../README.md#L12C3)",
          "[Absolute](C:/repo/docs/api.md#L7)",
        ].join("\n\n")}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "Sibling" }));
    fireEvent.click(screen.getByRole("link", { name: "Parent" }));
    fireEvent.click(screen.getByRole("link", { name: "Absolute" }));

    expect(onOpenSourceLink).toHaveBeenNthCalledWith(1, {
      path: "C:\\repo\\docs\\features\\guide.md",
      line: 10,
      openInNewTab: false,
    });
    expect(onOpenSourceLink).toHaveBeenNthCalledWith(2, {
      path: "C:\\repo\\docs\\README.md",
      line: 12,
      column: 3,
      openInNewTab: false,
    });
    expect(onOpenSourceLink).toHaveBeenNthCalledWith(3, {
      path: "C:/repo/docs/api.md",
      line: 7,
      openInNewTab: false,
    });
  });

  it("keeps document-relative links inside a Windows UNC workspace share", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        documentPath={String.raw`\\server\share\docs\features\intro.md`}
        markdown="[Sibling](../guide.md#L5)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot={String.raw`\\server\share`}
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "Sibling" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: String.raw`\\server\share\docs\guide.md`,
      line: 5,
      openInNewTab: false,
    });
  });

  it("does not create nested anchors for inline code inside Markdown links", () => {
    const { container } = render(
      <MarkdownContent
        markdown={"[prefix `lib/models/foo.rs` suffix](https://example.com)"}
        onOpenSourceLink={vi.fn()}
        workspaceRoot="/repo"
      />,
    );

    expect(container.querySelectorAll("a a")).toHaveLength(0);
    expect(screen.getByText("lib/models/foo.rs", { selector: "code" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /prefix lib\/models\/foo\.rs suffix/ })).toHaveAttribute(
      "href",
      "https://example.com",
    );
  });
});

describe("MarkdownContent line numbers", () => {
  it("renders source line numbers only when requested", async () => {
    const markdown = ["# Title", "", "Body text.", "", "- First item"].join("\n");

    const { container, rerender } = render(
      <MarkdownContent markdown={markdown} startLineNumber={40} />,
    );

    expect(container.querySelector("[data-markdown-line-start]")).toBeNull();
    expect(container.querySelector(".markdown-line-gutter")).toBeNull();
    expect(container.querySelector(".markdown-copy-shell-with-line-numbers")).toBeNull();

    rerender(
      <MarkdownContent markdown={markdown} showLineNumbers startLineNumber={40} />,
    );

    await waitFor(() => {
      expect(container.querySelector('[data-markdown-line-start="40"]')).not.toBeNull();
      expect(container.querySelector('[data-markdown-line-start="42"]')).not.toBeNull();
      expect(container.querySelector('[data-markdown-line-start="44"]')).not.toBeNull();
      expect(
        container.querySelector(".markdown-line-gutter [data-markdown-gutter-line='40']"),
      ).toHaveTextContent("40");
    });
    expect(container.querySelector(".markdown-copy")).not.toHaveTextContent("40");
    expect(container.querySelector("ul[data-markdown-line-start]")).toBeNull();
    expect(container.querySelector('li[data-markdown-line-start="44"]')).not.toBeNull();
  });

  it("labels multi-line fenced code blocks with the rendered source range", async () => {
    const { container } = render(
      <MarkdownContent
        markdown={["Intro", "", "```ts", "const a = 1;", "const b = 2;", "```"].join("\n")}
        showLineNumbers
        startLineNumber={20}
      />,
    );

    await waitFor(() => {
      const codeBlock = container.querySelector("pre[data-markdown-line-start='22']");
      expect(codeBlock).not.toBeNull();
      expect(codeBlock).toHaveAttribute("data-markdown-line-range", "22-25");
    });
  });

  it("does not emit duplicate line markers for nested block elements", async () => {
    const { container } = render(
      <MarkdownContent markdown={"- Item\n  > Nested quote\n"} showLineNumbers />,
    );

    await waitFor(() => {
      const markers = container.querySelectorAll("[data-markdown-line-start]");
      expect(markers).toHaveLength(1);
      expect(markers[0]).toHaveAttribute("data-markdown-line-start", "1");
    });
  });

  it("keeps the line-number observer stable across search-only rerenders", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    let observeCount = 0;
    let disconnectCount = 0;
    class ResizeObserverMock {
      observe() {
        observeCount += 1;
      }
      disconnect() {
        disconnectCount += 1;
      }
    }
    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      const markdown = ["# Title", "", "Body text."].join("\n");
      const { container, rerender } = render(
        <MarkdownContent markdown={markdown} showLineNumbers searchQuery="" />,
      );

      await waitFor(() => {
        expect(container.querySelector(".markdown-line-gutter [data-markdown-gutter-line='1']")).not.toBeNull();
      });
      expect(observeCount).toBe(1);
      expect(disconnectCount).toBe(0);

      rerender(
        <MarkdownContent markdown={markdown} showLineNumbers searchQuery="Body" />,
      );

      expect(observeCount).toBe(1);
      expect(disconnectCount).toBe(0);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
    }
  });

  it("updates gutter positions from ResizeObserver geometry changes", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    let resizeCallback: ResizeObserverCallback | null = null;
    let bodyTop = 130;

    class ResizeObserverMock {
      constructor(callback: ResizeObserverCallback) {
        resizeCallback = callback;
      }
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const top = element.classList.contains("markdown-copy")
        ? 100
        : element.dataset.markdownLineStart === "3"
          ? bodyTop
          : 100;
      return {
        bottom: top + 20,
        height: 20,
        left: 0,
        right: 200,
        top,
        width: 200,
        x: 0,
        y: top,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const { container } = render(
        <MarkdownContent markdown={"# Title\n\nBody text."} showLineNumbers />,
      );

      await waitFor(() => {
        expect(container.querySelector(".markdown-line-gutter [data-markdown-gutter-line='3']")).toHaveStyle({
          top: "40px",
        });
      });

      bodyTop = 154;
      await act(async () => {
        resizeCallback?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
      });

      await waitFor(() => {
        expect(container.querySelector(".markdown-line-gutter [data-markdown-gutter-line='3']")).toHaveStyle({
          top: "64px",
        });
      });
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("compares line marker arrays by line, range, and top", () => {
    const marker = { line: 4, range: "4-6", top: 20 };

    expect(areMarkdownLineMarkersEqual([marker], [{ line: 4, range: "4-6", top: 20 }])).toBe(true);
    expect(areMarkdownLineMarkersEqual([marker], [{ line: 5, range: "4-6", top: 20 }])).toBe(false);
    expect(areMarkdownLineMarkersEqual([marker], [{ line: 4, range: "4", top: 20 }])).toBe(false);
    expect(areMarkdownLineMarkersEqual([marker], [{ line: 4, range: "4-6", top: 21 }])).toBe(false);
  });
});

// Phase 1 of `docs/features/source-renderers.md`: Markdown content
// renders inline `$...$` and block `$$...$$` math via `remark-math` +
// `rehype-katex`, with KaTeX output wrapped so the rendered-Markdown
// diff editor's serializer treats the presentation layer as skip and
// preserves the underlying source.
describe("MarkdownContent math rendering", () => {
  it("renders inline $...$ math as a KaTeX span with skip-serialization wrapper", () => {
    const { container } = render(
      <MarkdownContent markdown="Inline math: $E = mc^2$ stays inline." />,
    );

    // rehype-katex emits `.math.math-inline` spans with the rendered
    // KaTeX HTML. Our custom `span` renderer wraps them with
    // contentEditable=false + data-markdown-serialization="skip".
    const mathSpans = container.querySelectorAll("span.math.math-inline");
    expect(mathSpans).toHaveLength(1);
    const mathSpan = mathSpans[0] as HTMLSpanElement;
    expect(mathSpan.getAttribute("data-markdown-serialization")).toBe("skip");
    expect(mathSpan.getAttribute("contenteditable")).toBe("false");
    // Inside the wrapper is a `.katex` element — the actual rendered
    // output. Its DOM shape is KaTeX-internal; we only assert that
    // something was rendered.
    expect(mathSpan.querySelector(".katex")).not.toBeNull();
  });

  it("renders $$...$$ block math as a KaTeX div with skip-serialization wrapper", () => {
    const { container } = render(
      <MarkdownContent
        markdown={`Here is a block equation:\n\n$$\n\\int_0^1 x^2 \\, dx = \\frac{1}{3}\n$$\n`}
      />,
    );

    // rehype-katex emits `.math.math-display` divs for block math.
    const blockDivs = container.querySelectorAll("div.math.math-display");
    expect(blockDivs).toHaveLength(1);
    const blockDiv = blockDivs[0] as HTMLDivElement;
    expect(blockDiv.getAttribute("data-markdown-serialization")).toBe("skip");
    expect(blockDiv.getAttribute("contenteditable")).toBe("false");
    expect(blockDiv.querySelector(".katex")).not.toBeNull();
  });

  it("preserves non-math `span` / `div` children unchanged", () => {
    // Regression guard: the custom span/div renderers intercept only
    // the rehype-katex-labeled elements. Other span / div content
    // (from GFM, autolink plugin, etc.) must not receive the
    // skip-serialization attribute — only the KaTeX output does.
    const { container } = render(
      <MarkdownContent markdown="A plain paragraph with `code` and **bold**." />,
    );

    // No math → no KaTeX wrappers.
    expect(container.querySelectorAll('[data-markdown-serialization="skip"]')).toHaveLength(0);
    // Body text is rendered.
    expect(container.textContent).toContain("A plain paragraph");
    expect(container.textContent).toContain("code");
    expect(container.textContent).toContain("bold");
  });

  it("renders malformed LaTeX without throwing (throwOnError: false)", () => {
    // A malformed expression like `$\frac{1}$` (missing second arg)
    // should render as a KaTeX error span (red, with title carrying
    // the error text), not crash the whole Markdown render. The
    // `throwOnError: false` option we pass to rehype-katex guarantees
    // this.
    expect(() => {
      render(<MarkdownContent markdown="Bad: $\\frac{1}$ still renders." />);
    }).not.toThrow();
    // Document still shows the surrounding prose.
    expect(screen.getByText(/still renders\./)).toBeInTheDocument();
  });

  it("renders many equations up to the per-document budget", () => {
    // Exactly at the threshold: 100 inline expressions should render
    // as normal KaTeX wrappers (the budget check is strictly
    // greater-than).
    const markdown = Array.from({ length: 100 }, (_, index) => `$x_{${index}}$`).join(" ");
    const { container } = render(<MarkdownContent markdown={markdown} />);
    const mathSpans = container.querySelectorAll("span.math.math-inline");
    expect(mathSpans).toHaveLength(100);
    // All of them have the skip attribute — none fell back to the
    // budget-exceeded branch.
    for (const span of mathSpans) {
      expect(span.getAttribute("data-markdown-serialization")).toBe("skip");
      expect(span.classList.contains("math-render-skipped")).toBe(false);
    }
  });

  it("falls back to budget-skipped rendering when a document exceeds the math count cap", () => {
    // Over the cap (100). Every math wrapper carries the
    // `math-render-skipped` marker and the `title` note, matching the
    // Mermaid-style render-budget fallback pattern.
    const markdown = Array.from({ length: 101 }, (_, index) => `$y_{${index}}$`).join(" ");
    const { container } = render(<MarkdownContent markdown={markdown} />);
    const renderedMath = container.querySelectorAll(".math.math-inline");
    expect(renderedMath).toHaveLength(101);
    for (const element of renderedMath) {
      expect(element.classList.contains("math-render-skipped")).toBe(true);
      expect(element.getAttribute("data-markdown-serialization")).toBe("skip");
      expect(element.getAttribute("title")).toMatch(
        /Math render skipped/i,
      );
    }
  });

  it("does not render `$` in code blocks as math", () => {
    // `remark-math` must not tokenize `$` inside fenced code blocks.
    // The budget counter (`countMathExpressions`) mirrors this
    // behavior — if it's wrong, the budget gate would trip on files
    // that have literal `$` in shell snippets. This test pins both:
    // the renderer does not produce math wrappers for code-block
    // content.
    const markdown = ["```bash", "echo $HOME $PATH $USER", "```"].join("\n");
    const { container } = render(<MarkdownContent markdown={markdown} />);
    expect(container.querySelectorAll("span.math, div.math")).toHaveLength(0);
    // The code block still renders the literal `$HOME` text.
    expect(container.textContent).toContain("$HOME");
  });

  it("stamps the block math div with line attributes for gutter navigation when showLineNumbers is on", () => {
    const { container } = render(
      <MarkdownContent
        markdown={`Intro line.\n\n$$\nx^2 + y^2 = z^2\n$$\n`}
        showLineNumbers
      />,
    );

    const blockDiv = container.querySelector("div.math.math-display") as HTMLDivElement | null;
    expect(blockDiv).not.toBeNull();
    // Line attributes land on the wrapper so clicking the equation
    // can navigate to the source line in the underlying Monaco
    // editor. The exact line is derived from `remark-math`'s mdast
    // sourcePosition; we just pin that SOME line marker is present.
    expect(blockDiv?.hasAttribute("data-markdown-line-start")).toBe(true);
  });
});
