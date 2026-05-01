import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import mermaid from "mermaid";
import { beforeEach, describe, expect, it, vi } from "vitest";

import * as connectionRetry from "./connection-retry";
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

// Wrap the real `connection-retry` module and turn
// `parseConnectionRetryNotice` into a spy that forwards to the
// real implementation. `MessageCard`'s text-message path calls
// this helper exactly once per render (see
// `ui/src/message-cards.tsx:~208`), so the spy's call count is
// a proxy for "how many times has the memoized `MessageCard`
// actually executed its render body". The `MessageCard default
// props stable across re-renders` test below uses this to
// assert that a parent re-render with identical props does NOT
// re-run the body (i.e., the memo hit). All other tests in the
// file pass the spy-wrapped real behavior through unchanged, so
// the mock is safe to apply at module scope.
vi.mock("./connection-retry", async () => {
  const actual =
    await vi.importActual<typeof connectionRetry>("./connection-retry");
  return {
    ...actual,
    parseConnectionRetryNotice: vi.fn(actual.parseConnectionRetryNotice),
  };
});

const parseConnectionRetryNoticeMock = vi.mocked(
  connectionRetry.parseConnectionRetryNotice,
);

const mermaidInitializeMock = vi.mocked(mermaid.initialize);
const mermaidRenderMock = vi.mocked(mermaid.render);

beforeEach(() => {
  parseConnectionRetryNoticeMock.mockClear();
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
    expect(link).toHaveAttribute("href", "#");
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
  it("renders heavy thinking markdown immediately when preferImmediateHeavyRender is enabled", async () => {
    const originalIntersectionObserver = globalThis.IntersectionObserver;
    const observe = vi.fn();
    const disconnect = vi.fn();
    const unobserve = vi.fn();
    const intersectionObserverMock = vi.fn(function IntersectionObserverMock() {
      return {
        disconnect,
        observe,
        unobserve,
      };
    });
    vi.stubGlobal(
      "IntersectionObserver",
      intersectionObserverMock as unknown as typeof IntersectionObserver,
    );
    const lines = [
      "```mermaid",
      "flowchart TD",
      ...Array.from({ length: 28 }, (_, index) => `  N${index} --> N${index + 1}`),
      "```",
    ];
    const message = {
      author: "assistant",
      id: "message-heavy-thinking",
      lines,
      timestamp: "2026-04-15T00:00:00.000Z",
      title: "Thinking",
      type: "thinking",
    } as const;
    const noop = () => {};

    try {
      render(
        <MessageCard
          message={message}
          onApprovalDecision={noop}
          onUserInputSubmit={noop}
          preferImmediateHeavyRender
        />,
      );

      expect(await screen.findByTestId("mermaid-frame")).toBeInTheDocument();
      expect(intersectionObserverMock).not.toHaveBeenCalled();
    } finally {
      if (originalIntersectionObserver === undefined) {
        Reflect.deleteProperty(globalThis, "IntersectionObserver");
      } else {
        globalThis.IntersectionObserver = originalIntersectionObserver;
      }
    }
  });

  it("renders code-heavy approval content immediately when preferImmediateHeavyRender is enabled", () => {
    const originalIntersectionObserver = globalThis.IntersectionObserver;
    const observe = vi.fn();
    const disconnect = vi.fn();
    const unobserve = vi.fn();
    const intersectionObserverMock = vi.fn(function IntersectionObserverMock() {
      return {
        disconnect,
        observe,
        unobserve,
      };
    });
    vi.stubGlobal(
      "IntersectionObserver",
      intersectionObserverMock as unknown as typeof IntersectionObserver,
    );
    const message = {
      author: "assistant",
      command: [
        "cargo test --workspace --all-features",
        ...Array.from({ length: 40 }, (_, index) => `echo line-${index}`),
      ].join("\n"),
      commandLanguage: "bash",
      decision: "pending",
      detail: "Run the full backend suite before merging.",
      id: "message-heavy-approval",
      timestamp: "2026-04-15T00:00:00.000Z",
      title: "Approval",
      type: "approval",
    } as const;
    const noop = () => {};

    try {
      const { container } = render(
        <MessageCard
          message={message}
          onApprovalDecision={noop}
          onUserInputSubmit={noop}
          preferImmediateHeavyRender
        />,
      );

      expect(
        container.querySelector(".deferred-code-placeholder"),
      ).toBeNull();
      expect(container.textContent).toContain(
        "cargo test --workspace --all-features",
      );
      expect(intersectionObserverMock).not.toHaveBeenCalled();
    } finally {
      if (originalIntersectionObserver === undefined) {
        Reflect.deleteProperty(globalThis, "IntersectionObserver");
      } else {
        globalThis.IntersectionObserver = originalIntersectionObserver;
      }
    }
  });

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
      handDrawnSeed: 42,
      htmlLabels: true,
      look: "classic",
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
    expect(widthPx).toBeGreaterThan(0);
    expect(widthPx).toBeLessThanOrEqual(4096);
    expect(frame.style.height).toBe("4096px");
    expect(frame.style.aspectRatio).toBe("");
    expect(frame.style.maxWidth).toBe("100%");
    // The inline width/height cap is the primary guard; also confirm the
    // container did not propagate the runaway SVG dimensions.
    expect(container.querySelector(".mermaid-diagram-frame")).toBe(frame);
  });

  it("encodes 24-px vertical slack in the Mermaid iframe height", async () => {
    // The iframe srcDoc CSS uses `overflow-x: auto; overflow-y: hidden`
    // so wide diagrams can scroll horizontally inside the iframe, but
    // the horizontal scrollbar eats ~16 px at the bottom of the frame.
    // The production code in `getMermaidDiagramFrameStyle` reserves
    // `Math.ceil(dimensions.height) + 24` to prevent the scrollbar
    // chrome from clipping the last row (and to absorb a few pixels
    // of render drift from the temp-DOM's font metrics). An earlier
    // version used `+ 8`; this test pins the `+ 24` contract so a
    // regression back to `+ 8` fails here instead of silently shipping
    // a diagram that looks one row short.
    mermaidRenderMock.mockResolvedValueOnce({
      diagramType: "flowchart",
      svg: '<svg data-testid="mermaid-svg" viewBox="0 0 300 80"><text>ok</text></svg>',
    });

    render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    const frame = await screen.findByTestId("mermaid-frame");
    const widthPx = Number.parseInt(frame.style.width, 10);
    // viewBox width 300 + 2 = 302; within [180, 4096] so not clamped.
    expect(widthPx).toBe(302);
    // viewBox height 80 + 24 slack = 104; kept as explicit iframe height
    // because wide diagrams scroll at intrinsic SVG size inside the frame.
    expect(frame.style.height).toBe("104px");
    expect(frame.style.aspectRatio).toBe("");
  });

  it("keeps wide Mermaid iframe height intrinsic when max-width constrains the frame", async () => {
    mermaidRenderMock.mockResolvedValueOnce({
      diagramType: "er",
      svg: '<svg data-testid="mermaid-svg" viewBox="0 0 2340.4453125 926.6875"><text>wide er</text></svg>',
    });

    render(
      <MarkdownContent
        markdown={[
          "```mermaid",
          "erDiagram",
          "  USERS {",
          "    uuid id",
          "  }",
          "```",
        ].join("\n")}
      />,
    );

    const frame = await screen.findByTestId("mermaid-frame");
    expect(frame.style.width).toBe("2343px");
    expect(frame.style.maxWidth).toBe("100%");
    expect(frame.style.height).toBe("951px");
    expect(frame.style.aspectRatio).toBe("");
  });

  it("clamps Mermaid iframe dimensions up to the lower bound when the viewBox is negative", async () => {
    // `readMermaidSvgDimensions` accepts signed values via its
    // `[-+]?` regex and `Number.isFinite` check, so a negative
    // viewBox threads through to the clamp. The clamp's lower bound
    // (180 width, 60 height) keeps the frame legible even when the
    // SVG reports nonsense. Protects against hostile or buggy agent
    // output producing `viewBox="0 0 -W -H"`.
    mermaidRenderMock.mockResolvedValueOnce({
      diagramType: "flowchart",
      svg: '<svg data-testid="mermaid-svg" viewBox="0 0 -100 -100"><text>negative</text></svg>',
    });

    render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    const frame = await screen.findByTestId("mermaid-frame");
    const widthPx = Number.parseInt(frame.style.width, 10);
    expect(widthPx).toBe(180);
    expect(frame.style.height).toBe("60px");
    expect(frame.style.aspectRatio).toBe("");
  });

  it("clamps Mermaid iframe dimensions up to the lower bound when the viewBox is zero", async () => {
    // Similar lower-clamp case but with a `0 0` viewBox — this can
    // happen when Mermaid renders an empty or failed diagram whose
    // temp-DOM reports zero dimensions. The frame must still be
    // visible at the lower-bound size so the rendered content (or
    // the Mermaid error overlay) is reachable.
    mermaidRenderMock.mockResolvedValueOnce({
      diagramType: "flowchart",
      svg: '<svg data-testid="mermaid-svg" viewBox="0 0 0 0"><text>zero</text></svg>',
    });

    render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    const frame = await screen.findByTestId("mermaid-frame");
    const widthPx = Number.parseInt(frame.style.width, 10);
    expect(widthPx).toBe(180);
    expect(frame.style.height).toBe("60px");
    expect(frame.style.aspectRatio).toBe("");
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

  // Pins the actual memo-hit behaviour for the omitted-optional-
  // callback case. docs/bugs.md → "`MessageCard` default-prop
  // inline arrows" originally claimed that inline arrow defaults
  // in the destructuring head (`onMcpElicitationSubmit = () =>
  // {}`) defeated memoization by making the comparator see a
  // fresh-function-per-render, but that's a misdiagnosis —
  // React's `memo` comparator receives the RAW props object as
  // passed by the parent, NOT the destructured values. When the
  // parent omits an optional prop, both `prev` and `next` read
  // as `undefined` and pass the `===` identity check cleanly.
  //
  // The test is still useful as a memo regression guard: it
  // counts how many times `parseConnectionRetryNotice` runs
  // (called once per text-message render body) across a parent
  // re-render with identical props, and asserts the count does
  // NOT go up. A future regression to the memo comparator (e.g.,
  // forgetting to compare a new prop, or inverting a check) that
  // breaks the omitted-optional path specifically would trip
  // this. The fix itself (module-scope stable no-op constants)
  // is a code-quality + tiny-GC improvement, not a correctness
  // requirement — so this test would pass both with the fix
  // applied AND with the original inline arrow defaults.
  it("skips re-rendering when a parent re-renders with identical props and no optional callbacks", () => {
    const onApprovalDecision = vi.fn();
    const onUserInputSubmit = vi.fn();
    const message = {
      author: "assistant" as const,
      id: "text-memo-test",
      text: "plain text body",
      timestamp: "2026-04-15T00:00:00.000Z",
      type: "text" as const,
    };
    parseConnectionRetryNoticeMock.mockClear();

    const { rerender } = render(
      <MessageCard
        message={message}
        onApprovalDecision={onApprovalDecision}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );
    const callsAfterMount = parseConnectionRetryNoticeMock.mock.calls.length;
    expect(callsAfterMount).toBeGreaterThan(0);

    // Rerender with IDENTICAL props. The parent (this test) has
    // no state change, and all props are the same references
    // (`message`, `onApprovalDecision`, `onUserInputSubmit`).
    // The two optional callbacks (`onMcpElicitationSubmit`,
    // `onCodexAppRequestSubmit`) are omitted and must resolve to
    // the same module-scope NOOP references as before. If the
    // memo comparator sees every listed prop as `===` equal, the
    // render body is skipped → no new call to
    // `parseConnectionRetryNotice`.
    rerender(
      <MessageCard
        message={message}
        onApprovalDecision={onApprovalDecision}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );

    expect(parseConnectionRetryNoticeMock.mock.calls.length).toBe(callsAfterMount);
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

// Dangerous-protocol neutralization integration test. The unit-level
// contract for `transformMarkdownLinkUri` lives in
// `ui/src/markdown-links.test.ts`; this covers the rendered-DOM side
// of the pipeline: an empty `href` (the sentinel our transform emits
// for neutralized URIs) must render as a plain `<span>`, not an
// anchor, so there's no inert same-page-navigate, no
// `javascript:void(0)` reaching the DOM, and no React warning.
describe("MarkdownContent dangerous link neutralization", () => {
  it("renders `[text](javascript:alert(1))` as plain text with no anchor", () => {
    render(<MarkdownContent markdown="[click me](javascript:alert(1))" />);

    // The rendered text is still visible — we don't silently drop
    // the link label, we just render it without an anchor.
    expect(screen.getByText("click me")).toBeInTheDocument();

    // No link role — the `a`-renderer took the empty-href branch and
    // returned a `<span>`. If this flips, a regression has broken
    // either `transformMarkdownLinkUri`'s sentinel substitution or
    // the `!href` guard in `MarkdownContent`'s `a` component.
    expect(screen.queryByRole("link", { name: /click me/ })).toBeNull();

    // Load-bearing DOM-level assertion: the
    // `"javascript:void(0)"` placeholder that react-markdown uses
    // internally must not reach the DOM. React ≥ 18.3 logs a warning
    // if it does, and is slated to block the string outright in a
    // future release. This is the real invariant — even if the
    // internal sentinel string changes upstream, this check keeps
    // dangerous hrefs out of the rendered output.
    expect(document.querySelector('[href="javascript:void(0)"]')).toBeNull();
    // Belt-and-braces: no anchor with a `javascript:` prefix at all.
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
  });

  it("neutralizes `vbscript:` and non-image `data:` Markdown links the same way", () => {
    render(
      <MarkdownContent
        markdown={[
          "[vb](vbscript:msgbox('x'))",
          "[html](data:text/html,<script>alert(1)</script>)",
        ].join("\n\n")}
      />,
    );

    expect(screen.getByText("vb")).toBeInTheDocument();
    expect(screen.getByText("html")).toBeInTheDocument();
    expect(screen.queryAllByRole("link")).toHaveLength(0);
    expect(document.querySelectorAll('[href="javascript:void(0)"]')).toHaveLength(0);
    expect(document.querySelectorAll('a[href^="javascript:"]')).toHaveLength(0);
    expect(document.querySelectorAll('a[href^="vbscript:"]')).toHaveLength(0);
    expect(document.querySelectorAll('a[href^="data:"]')).toHaveLength(0);
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

// The following suite was relocated from App.test.tsx in Slice 2 of
// the App-split plan (see docs/app-split-plan.md). It intentionally
// keeps the original top-level `describe("MarkdownContent", ...)`
// name so the tests remain grep-findable by their historical
// identifiers.
describe("MarkdownContent", () => {
  it("wraps markdown tables in a scroll container", () => {
    const markdown = [
      "| Finding | Resolution |",
      "| --- | --- |",
      "| `skip_list.rs` | Fixed |",
    ].join("\n");

    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});

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

  // Streaming-aware partial-block deferral. The four progressive
  // states a streaming pipe-table passes through (header alone,
  // header + separator, header + separator + partial body row,
  // settled with trailing blank line) must each render a stable
  // shape — either the in-flight `<pre class="markdown-streaming-
  // fragment">` placeholder, or a full `<table>` once settled.
  // Without the deferral the user observed visibly-broken
  // intermediate shapes (raw `| ... |` text, table rows with
  // mismatched cell counts) flickering on every textDelta.
  describe("isStreaming partial-table deferral", () => {
    it("renders the header alone as a streaming placeholder, not as a table", () => {
      const markdown = "| Col A | Col B |";
      const { container } = render(
        <MarkdownContent isStreaming markdown={markdown} />,
      );
      expect(container.querySelector(".markdown-table-scroll")).toBeNull();
      const fragment = container.querySelector(".markdown-streaming-fragment");
      expect(fragment).not.toBeNull();
      expect(fragment?.textContent).toBe(markdown);
    });

    it("defers header + complete separator (no body row yet) to the placeholder", () => {
      const markdown = "| Col A | Col B |\n| --- | --- |";
      const { container } = render(
        <MarkdownContent isStreaming markdown={markdown} />,
      );
      expect(container.querySelector(".markdown-table-scroll")).toBeNull();
      expect(
        container.querySelector(".markdown-streaming-fragment")?.textContent,
      ).toBe(markdown);
    });

    it("defers header + separator + partial body row to the placeholder", () => {
      const markdown = "| Col A | Col B |\n| --- | --- |\n| 42 |";
      const { container } = render(
        <MarkdownContent isStreaming markdown={markdown} />,
      );
      expect(container.querySelector(".markdown-table-scroll")).toBeNull();
      expect(
        container.querySelector(".markdown-streaming-fragment")?.textContent,
      ).toBe(markdown);
    });

    it("snaps to a full <table> once a trailing blank line settles the block", () => {
      const markdown = "| Col A | Col B |\n| --- | --- |\n| 1 | 2 |\n";
      const { container } = render(
        <MarkdownContent isStreaming markdown={markdown} />,
      );
      const tableScroll = container.querySelector(".markdown-table-scroll");
      expect(tableScroll).not.toBeNull();
      expect(tableScroll?.querySelector("table")).not.toBeNull();
      expect(
        container.querySelector(".markdown-streaming-fragment"),
      ).toBeNull();
    });

    it("renders the settled prefix as Markdown alongside a deferred trailing partial table", () => {
      // Realistic mid-stream shape: a paragraph has settled, then a
      // table starts streaming. The paragraph must render through
      // the full Markdown pipeline; only the partial table goes
      // into the streaming-fragment placeholder.
      const markdown =
        "Here is the result:\n\n| Col A | Col B |\n| --- | --- |\n| 1 |";
      const { container } = render(
        <MarkdownContent isStreaming markdown={markdown} />,
      );

      // Paragraph rendered as Markdown
      expect(
        container.querySelector(".markdown-copy")?.querySelector("p")
          ?.textContent,
      ).toBe("Here is the result:");

      // Table NOT rendered as a real <table>
      expect(container.querySelector(".markdown-table-scroll")).toBeNull();

      // Partial table held in the placeholder
      expect(
        container.querySelector(".markdown-streaming-fragment")?.textContent,
      ).toBe("| Col A | Col B |\n| --- | --- |\n| 1 |");
    });

    it("does not defer settled callers that omit isStreaming", () => {
      // Default behavior is unchanged: a static (non-streaming)
      // markdown payload that ends mid-table — e.g., a stored
      // history message that happens to lack a trailing blank
      // line — still goes through the full pipeline. This
      // preserves the existing rendering for source-renderer
      // previews, diff views, and settled history bubbles.
      const markdown = "| Col A | Col B |\n| --- | --- |\n| 1 | 2 |";
      const { container } = render(<MarkdownContent markdown={markdown} />);
      expect(
        container.querySelector(".markdown-streaming-fragment"),
      ).toBeNull();
      expect(container.querySelector(".markdown-table-scroll")).not.toBeNull();
    });

    it("defers an unclosed fenced code block during streaming", () => {
      const markdown = "Intro.\n\n```js\nconsole.log(";
      const { container } = render(
        <MarkdownContent isStreaming markdown={markdown} />,
      );
      // The intro paragraph still renders.
      expect(
        container.querySelector(".markdown-copy")?.querySelector("p")
          ?.textContent,
      ).toBe("Intro.");
      // The unclosed fence is held as plain text in the placeholder.
      expect(
        container.querySelector(".markdown-streaming-fragment")?.textContent,
      ).toBe("```js\nconsole.log(");
    });
  });

  it("opens local file links through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[experience.tex#L63](experience.tex#L63)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("opens absolute Windows file links through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[route_post_processing_service.dart:469](C:/github/Personal/fit_friends/lib/services/route_post_processing_service.dart#L469)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/github/Personal/TermAl"
      />,
    );

    fireEvent.click(
      screen.getByRole("link", {
        name: "route_post_processing_service.dart:469",
      }),
    );

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "C:/github/Personal/fit_friends/lib/services/route_post_processing_service.dart",
      line: 469,
      openInNewTab: false,
    });
  });

  it("keeps encoded Windows file links inert while routing through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[README](C:%5Crepo%5Cdocs%5CREADME.md#L9)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/repo"
      />,
    );

    const link = screen.getByText("README").closest("a");
    expect(link).not.toBeNull();
    expect(link).toHaveAttribute("href", "#");
    expect(link).toHaveAttribute(
      "data-markdown-link-href",
      "C:%5Crepo%5Cdocs%5CREADME.md#L9",
    );
    expect(link).not.toHaveAttribute("target");

    fireEvent.click(link!);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: String.raw`C:\repo\docs\README.md`,
      line: 9,
      openInNewTab: false,
    });
  });

  it("does not expose encoded Windows file links as DOM c-scheme hrefs without a source callback", () => {
    render(
      <MarkdownContent
        markdown="[README](C:%5Crepo%5Cdocs%5CREADME.md)"
        workspaceRoot="C:/repo"
      />,
    );

    const link = screen.getByText("README").closest("a");
    expect(link).not.toBeNull();
    expect(link).toHaveAttribute("href", "#");
    expect(document.querySelector('a[href^="c:"]')).toBeNull();

    const clickEvent = new MouseEvent("click", {
      bubbles: true,
      cancelable: true,
    });
    link!.dispatchEvent(clickEvent);

    expect(clickEvent.defaultPrevented).toBe(true);
  });

  it("scrubs encoded Windows file-link hrefs even when source resolution fails", () => {
    render(
      <MarkdownContent
        markdown="[README](C:%5Coutside%5CREADME.md)"
        workspaceRoot="D:/repo"
      />,
    );

    const link = screen.getByRole("link", { name: "README" });
    expect(link).toHaveAttribute("href", "#");
    expect(link).toHaveAttribute(
      "data-markdown-link-href",
      "C:%5Coutside%5CREADME.md",
    );
    expect(document.querySelector('a[href^="c:"]')).toBeNull();

    const clickEvent = new MouseEvent("click", {
      bubbles: true,
      cancelable: true,
    });
    link.dispatchEvent(clickEvent);

    expect(clickEvent.defaultPrevented).toBe(true);
  });

  it("opens absolute Linux file links through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[route_post_processing_service.dart:469](/home/grzeg/projects/fit_friends/lib/services/route_post_processing_service.dart#L469)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(
      screen.getByRole("link", {
        name: "route_post_processing_service.dart:469",
      }),
    );

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/home/grzeg/projects/fit_friends/lib/services/route_post_processing_service.dart",
      line: 469,
      openInNewTab: false,
    });
  });

  it("opens localhost app file URLs through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[20260322000004_child_provisioning_rpcs.sql](http://127.0.0.1:4173/C:/github/Personal/questly/supabase/migrations/20260322000004_child_provisioning_rpcs.sql#L15C1)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/github/Personal/questly"
      />,
    );

    const link = screen.getByRole("link", {
      name: "20260322000004_child_provisioning_rpcs.sql",
    });
    expect(link).not.toHaveAttribute("target");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "C:/github/Personal/questly/supabase/migrations/20260322000004_child_provisioning_rpcs.sql",
      line: 15,
      column: 1,
      openInNewTab: false,
    });
  });

  it("renders bare localhost app file URLs with workspace-relative labels", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="http://127.0.0.1:4173/C:/github/Personal/questly/supabase/migrations/20260322000004_child_provisioning_rpcs.sql#L15C1"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/github/Personal/questly"
      />,
    );

    const link = screen.getByRole("link", {
      name: "supabase/migrations/20260322000004_child_provisioning_rpcs.sql#L15C1",
    });
    expect(link).not.toHaveAttribute("target");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "C:/github/Personal/questly/supabase/migrations/20260322000004_child_provisioning_rpcs.sql",
      line: 15,
      column: 1,
      openInNewTab: false,
    });
  });

  it("opens localhost Unix file URLs through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="[service.rs](http://127.0.0.1:4173/home/grzeg/projects/fit_friends/src/service.rs#L12)"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/home/grzeg/projects/fit_friends"
      />,
    );

    const link = screen.getByRole("link", { name: "service.rs" });
    expect(link).not.toHaveAttribute("target");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/home/grzeg/projects/fit_friends/src/service.rs",
      line: 12,
      openInNewTab: false,
    });
  });
  it("keeps same-origin docs URLs as normal external links", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="http://localhost/docs/architecture.md"
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const link = screen.getByRole("link", {
      name: "http://localhost/docs/architecture.md",
    });
    expect(link).toHaveAttribute("target", "_blank");

    fireEvent.click(link);

    expect(onOpenSourceLink).not.toHaveBeenCalled();
  });

  it("autolinks bare file references with line targets", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="The Microsoft scope bullet needs more evidence in experience.tex#L63."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("autolinks bare file references with dotted line targets", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="The Microsoft scope bullet needs more evidence in experience.tex.#L63."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex.#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("opens inline code file references through the source callback", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="Text like `experience.tex.#L63` should stay clickable."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex.#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });
});
