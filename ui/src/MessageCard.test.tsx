import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  DeferredHeavyContentActivationProvider,
  MessageCard,
} from "./message-cards";
import {
  DEFERRED_RENDER_RESUME_EVENT,
  DEFERRED_RENDER_SUSPENDED_ATTRIBUTE,
} from "./deferred-render";
import type {
  ApprovalMessage,
  CodexAppRequestMessage,
  DiffMessage,
  FileChangesMessage,
  McpElicitationRequestMessage,
  TextMessage,
  ThinkingMessage,
  UserInputRequestMessage,
} from "./types";

describe("MessageCard", () => {
  it("shows a command badge for slash-expanded prompts", () => {
    const message: TextMessage = {
      id: "message-1",
      type: "text",
      author: "you",
      timestamp: "10:00",
      text: "/review-local",
      expandedText: "Review staged and unstaged changes.",
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);

    expect(screen.getByText("Command")).toBeInTheDocument();
  });

  it("does not show a command badge for regular user prompts", () => {
    const message: TextMessage = {
      id: "message-2",
      type: "text",
      author: "you",
      timestamp: "10:01",
      text: "Please inspect the changes.",
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);

    expect(screen.queryByText("Command")).not.toBeInTheDocument();
  });

  it("renders thinking content with markdown formatting", async () => {
    const message: ThinkingMessage = {
      id: "message-3",
      type: "thinking",
      author: "assistant",
      timestamp: "10:02",
      title: "Thinking",
      lines: ["## Summary of Changes", "1. Added markdown rendering", "- Preserved list formatting"],
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);

    expect(await screen.findByRole("heading", { name: "Summary of Changes" })).toBeInTheDocument();
    expect(screen.getByText("Added markdown rendering")).toBeInTheDocument();
    expect(screen.getByText("Preserved list formatting")).toBeInTheDocument();
  });

  it("renders markdown for active streaming assistant text that contains markdown structure", async () => {
    const message: TextMessage = {
      id: "message-streaming",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: "## Summary of Changes\n- Render markdown while streaming\n\nUse `code` and **bold**.",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        preferStreamingPlainTextRender
      />,
    );

    expect(
      await screen.findByRole("heading", { name: "Summary of Changes" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Render markdown while streaming"),
    ).toBeInTheDocument();
    expect(screen.getByText("code").tagName).toBe("CODE");
    expect(screen.getByText("bold").tagName).toBe("STRONG");
    expect(container.querySelector(".plain-text-copy")).toBeNull();
  });

  it("keeps incremental streaming bug-count markdown as a list", () => {
    const baseMessage: TextMessage = {
      id: "message-streaming-bug-count",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: "Active bug in `docs/bugs.md`:\n\n- High",
    };

    const { container, rerender } = render(
      <MessageCard
        message={baseMessage}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        preferStreamingPlainTextRender
      />,
    );

    rerender(
      <MessageCard
        message={{
          ...baseMessage,
          text: [
            "Active bug in `docs/bugs.md`:",
            "",
            "- High: 2",
            "- Medium: 24",
            "- Low: 35",
            "- Note: 2",
            "- Total: 63",
          ].join("\n"),
        }}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        preferStreamingPlainTextRender
      />,
    );

    expect(container.querySelector(".plain-text-copy")).toBeNull();
    expect(screen.getByText("docs/bugs.md").tagName).toBe("CODE");
    expect(screen.getByRole("list")).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(5);
    expect(screen.getByText("High: 2")).toBeInTheDocument();
    expect(screen.getByText("Medium: 24")).toBeInTheDocument();
    expect(screen.getByText("Low: 35")).toBeInTheDocument();
    expect(screen.getByText("Note: 2")).toBeInTheDocument();
    expect(screen.getByText("Total: 63")).toBeInTheDocument();
  });

  it("routes tilde-fenced streaming assistant text through the pending markdown placeholder", () => {
    const message: TextMessage = {
      id: "message-streaming-tilde-fence",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: "~~~ts\nconst answer = 42;",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        preferStreamingPlainTextRender
      />,
    );

    const pendingFragment = container.querySelector(
      ".markdown-streaming-fragment",
    );
    expect(pendingFragment).not.toBeNull();
    expect(pendingFragment).toHaveTextContent("~~~ts");
    expect(pendingFragment).toHaveTextContent("const answer = 42;");
    expect(container.querySelector(".plain-text-copy")).toBeNull();
  });

  it("routes active streaming pipe tables through the pending markdown placeholder", () => {
    const message: TextMessage = {
      id: "message-streaming-table",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: [
        "Tracked Project Total",
        "",
        "| Group | Files | Lines | Size |",
        "| --- | ---: | ---: | ---: |",
        "| Backend | 107 | 87,395 | 3.19 MiB |",
      ].join("\n"),
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        preferStreamingPlainTextRender
      />,
    );

    expect(container.querySelector(".plain-text-copy")).toBeNull();
    expect(container.querySelector(".markdown-table-scroll")).toBeNull();
    expect(
      container.querySelector(".markdown-copy")?.querySelector("p")
        ?.textContent,
    ).toBe("Tracked Project Total");
    expect(
      container.querySelector(".markdown-streaming-fragment")?.textContent,
    ).toBe(
      [
        "| Group | Files | Lines | Size |",
        "| --- | ---: | ---: | ---: |",
        "| Backend | 107 | 87,395 | 3.19 MiB |",
      ].join("\n"),
    );
  });

  it("routes active streaming standalone display math through the pending markdown placeholder", () => {
    const message: TextMessage = {
      id: "message-streaming-display-math",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: "Equation:\n\n$$\n\\sum_{i=1}^n i",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        preferStreamingPlainTextRender
      />,
    );

    expect(container.querySelector(".plain-text-copy")).toBeNull();
    expect(container.querySelector(".math.math-display")).toBeNull();
    expect(
      container.querySelector(".markdown-copy")?.querySelector("p")
        ?.textContent,
    ).toBe("Equation:");
    expect(
      container.querySelector(".markdown-streaming-fragment")?.textContent,
    ).toBe("$$\n\\sum_{i=1}^n i");
  });

  it("renders streaming assistant prose without markdown through the unified Markdown pipeline", () => {
    // Earlier revisions used a bare-`<p>` `StreamingAssistantTextShell`
    // fast path for streaming assistant text that had not yet accrued
    // Markdown structure. That fast path has been removed so the
    // rendered React subtree stays stable across the moment when the
    // first `**`, `# `, `- `, etc. arrives mid-stream — preventing the
    // visible flicker of unmounting the shell `<p>` and mounting the
    // full Markdown subtree at the same JSX position. Plain prose now
    // renders through `<MarkdownContent>` (which itself produces a
    // `<p>` for prose), so the assistant's text appears inside the
    // canonical `.markdown-copy` wrapper instead of a separate
    // `.plain-text-copy` element.
    const message: TextMessage = {
      id: "message-streaming-plain",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: "Checking the workspace and waiting for the next chunk of output.",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        preferStreamingPlainTextRender
      />,
    );

    const markdownCopy = container.querySelector(".markdown-copy");
    expect(markdownCopy).not.toBeNull();
    expect(markdownCopy?.querySelector("p")?.textContent).toBe(
      "Checking the workspace and waiting for the next chunk of output.",
    );
    // Confirm the bare-`<p>` shell is gone: no `.plain-text-copy` is
    // emitted for an assistant message whose body is plain prose.
    expect(
      container.querySelector("article.bubble-assistant .plain-text-copy"),
    ).toBeNull();
  });

  it("keeps full markdown rendering for settled assistant text", async () => {
    const message: TextMessage = {
      id: "message-settled",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: "## Summary of Changes\n- Render markdown after the turn settles",
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: "Summary of Changes" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Render markdown after the turn settles"),
    ).toBeInTheDocument();
  });

  it("keeps heavy markdown deferred when activation is disabled", () => {
    const message: TextMessage = {
      id: "message-heavy-deferred",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: [
        "# Deferred heading",
        ...Array.from({ length: 28 }, (_, index) => `Line ${index + 1}`),
      ].join("\n"),
    };

    const { container } = render(
      <DeferredHeavyContentActivationProvider allowActivation={false}>
        <MessageCard
          message={message}
          onApprovalDecision={vi.fn()}
          onUserInputSubmit={vi.fn()}
        />
      </DeferredHeavyContentActivationProvider>,
    );

    expect(
      screen.queryByRole("heading", { name: "Deferred heading" }),
    ).not.toBeInTheDocument();
    expect(
      container.querySelector(".deferred-markdown-placeholder"),
    ).toBeInTheDocument();
  });

  it("keeps heavy markdown deferred while the message stack suspends activation", async () => {
    const message: TextMessage = {
      id: "message-heavy-scroll-suspended",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: [
        "# Deferred heading",
        ...Array.from({ length: 28 }, (_, index) => `Line ${index + 1}`),
      ].join("\n"),
    };

    const { container } = render(
      <div
        className="message-stack"
        {...{ [DEFERRED_RENDER_SUSPENDED_ATTRIBUTE]: "true" }}
      >
        <MessageCard
          message={message}
          onApprovalDecision={vi.fn()}
          onUserInputSubmit={vi.fn()}
        />
      </div>,
    );

    expect(
      screen.queryByRole("heading", { name: "Deferred heading" }),
    ).not.toBeInTheDocument();
    expect(
      container.querySelector(".deferred-markdown-placeholder"),
    ).toBeInTheDocument();

    const root = container.querySelector(".message-stack");
    expect(root).not.toBeNull();
    root!.removeAttribute(DEFERRED_RENDER_SUSPENDED_ATTRIBUTE);
    root!.dispatchEvent(new Event(DEFERRED_RENDER_RESUME_EVENT));

    expect(
      await screen.findByRole("heading", { name: "Deferred heading" }),
    ).toBeInTheDocument();
  });

  it("keeps immediate heavy assistant markdown mounted when scrolling disables immediate preference", async () => {
    const message: TextMessage = {
      id: "message-heavy-immediate",
      type: "text",
      author: "assistant",
      timestamp: "10:02",
      text: [
        "# Stable heading",
        ...Array.from({ length: 28 }, (_, index) => `Line ${index + 1}`),
      ].join("\n"),
    };

    const { container, rerender } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        preferImmediateHeavyRender
      />,
    );

    expect(
      await screen.findByRole("heading", { name: "Stable heading" }),
    ).toBeInTheDocument();

    rerender(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Stable heading" }),
    ).toBeInTheDocument();
    expect(
      container.querySelector(".deferred-markdown-placeholder"),
    ).not.toBeInTheDocument();
  });

  it("shows relative paths and diff counts for diff cards", () => {
    const message: DiffMessage = {
      id: "message-4",
      type: "diff",
      author: "assistant",
      timestamp: "10:03",
      changeType: "edit",
      diff: [
        "@@ -1,3 +1,4 @@",
        "-before",
        "+after",
        " shared",
        "+extra",
        " shared-again",
        "-deleted",
      ].join("\n"),
      filePath: "/repo/src/app.ts",
      summary: "Updated app.ts",
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        workspaceRoot="/repo"
      />,
    );

    expect(screen.getByText("src/app.ts")).toBeInTheDocument();
    expect(screen.queryByText("/repo/src/app.ts")).not.toBeInTheDocument();
    expect(screen.getByText("+2")).toBeInTheDocument();
    expect(screen.getByText("-2")).toBeInTheDocument();
  });

  it("renders agent changed files with open actions", () => {
    const onOpenSourceLink = vi.fn();
    const message: FileChangesMessage = {
      id: "message-files",
      type: "fileChanges",
      author: "assistant",
      timestamp: "10:04",
      title: "Agent changed 2 files",
      files: [
        { path: "/repo/src/app.ts", kind: "modified" },
        { path: "/repo/src/new.ts", kind: "created" },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onOpenSourceLink={onOpenSourceLink}
        onUserInputSubmit={vi.fn()}
        workspaceRoot="/repo"
      />,
    );

    expect(screen.getByText("Agent changed 2 files")).toBeInTheDocument();
    expect(screen.getByText("src/app.ts")).toBeInTheDocument();
    expect(screen.getByText("src/new.ts")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Open src/app.ts" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/src/app.ts",
      openInNewTab: false,
    });
  });

  it("collapses long agent changed file lists until expanded", () => {
    const message: FileChangesMessage = {
      id: "message-files-long",
      type: "fileChanges",
      author: "assistant",
      timestamp: "10:04",
      title: "Agent changed 7 files",
      files: Array.from({ length: 7 }, (_, index) => ({
        path: `/repo/src/file-${index + 1}.ts`,
        kind: "modified" as const,
      })),
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onOpenSourceLink={vi.fn()}
        onUserInputSubmit={vi.fn()}
        workspaceRoot="/repo"
      />,
    );

    expect(screen.getByText("Agent changed 7 files")).toBeInTheDocument();
    expect(screen.queryByText("src/file-1.ts")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Expand changed files" }));

    expect(screen.getByRole("button", { name: "Open src/file-1.ts" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy src/file-1.ts" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Collapse changed files" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Collapse changed files" }));

    expect(screen.queryByText("src/file-1.ts")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Expand changed files" })).toBeInTheDocument();
  });

  it("renders six changed files expanded without a collapse control", () => {
    const message: FileChangesMessage = {
      id: "message-files-threshold",
      type: "fileChanges",
      author: "assistant",
      timestamp: "10:04",
      title: "Agent changed 6 files",
      files: Array.from({ length: 6 }, (_, index) => ({
        path: `/repo/src/file-${index + 1}.ts`,
        kind: "modified" as const,
      })),
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onOpenSourceLink={vi.fn()}
        onUserInputSubmit={vi.fn()}
        workspaceRoot="/repo"
      />,
    );

    expect(screen.getByRole("button", { name: "Open src/file-1.ts" })).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Expand changed files" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Collapse changed files" }),
    ).not.toBeInTheDocument();
  });

  it("auto-expands long changed file lists during search without mutating collapse state", () => {
    const message: FileChangesMessage = {
      id: "message-files-search",
      type: "fileChanges",
      author: "assistant",
      timestamp: "10:04",
      title: "Agent changed 7 files",
      files: Array.from({ length: 7 }, (_, index) => ({
        path: `/repo/src/file-${index + 1}.ts`,
        kind: "modified" as const,
      })),
    };
    const { rerender } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onOpenSourceLink={vi.fn()}
        onUserInputSubmit={vi.fn()}
        searchQuery="file"
        workspaceRoot="/repo"
      />,
    );

    expect(screen.getByRole("button", { name: "Open src/file-1.ts" })).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /changed files/i }),
    ).not.toBeInTheDocument();

    rerender(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onOpenSourceLink={vi.fn()}
        onUserInputSubmit={vi.fn()}
        searchQuery=""
        workspaceRoot="/repo"
      />,
    );

    expect(screen.queryByText("src/file-1.ts")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Expand changed files" })).toBeInTheDocument();
  });

  it("shows canceled approvals as a resolved decision", () => {
    const message: ApprovalMessage = {
      id: "message-4",
      type: "approval",
      author: "assistant",
      timestamp: "10:03",
      title: "Approve edit",
      command: "apply_patch",
      detail: "Claude withdrew the request.",
      decision: "canceled",
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} onUserInputSubmit={vi.fn()} />);

    expect(screen.getByText("Decision: canceled")).toBeInTheDocument();
  });

  it("submits structured user input answers", () => {
    const onUserInputSubmit = vi.fn();
    const message: UserInputRequestMessage = {
      id: "message-user-input",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:04",
      title: "Codex needs input",
      detail: "Codex requested additional input for 2 questions.",
      state: "pending",
      questions: [
        {
          header: "Environment",
          id: "environment",
          question: "Which environment should I use?",
          options: [
            { label: "Production", description: "Use production." },
            { label: "Staging", description: "Use staging." },
          ],
        },
        {
          header: "API token",
          id: "apiToken",
          question: "Paste the temporary token.",
          isSecret: true,
        },
      ],
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={onUserInputSubmit}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: /Production/ }));
    fireEvent.change(screen.getByDisplayValue(""), {
      target: { value: "secret-123" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Submit answers" }));

    expect(onUserInputSubmit).toHaveBeenCalledWith("message-user-input", {
      environment: ["Production"],
      apiToken: ["secret-123"],
    });

    expect(screen.queryByText("Answer \"Environment\" before submitting.")).not.toBeInTheDocument();
  });

  it("submits MCP elicitation form content", () => {
    const onMcpElicitationSubmit = vi.fn();
    const message: McpElicitationRequestMessage = {
      id: "message-mcp",
      type: "mcpElicitationRequest",
      author: "assistant",
      timestamp: "10:05",
      title: "Codex needs MCP input",
      detail: "MCP server deployment-helper requested additional structured input.",
      state: "pending",
      request: {
        threadId: "thread-1",
        turnId: "turn-1",
        serverName: "deployment-helper",
        mode: "form",
        message: "Confirm the deployment settings.",
        requestedSchema: {
          type: "object",
          properties: {
            environment: {
              type: "string",
              title: "Environment",
              oneOf: [
                { const: "production", title: "Production" },
                { const: "staging", title: "Staging" },
              ],
            },
            replicas: {
              type: "integer",
              title: "Replicas",
            },
            notify: {
              type: "boolean",
              title: "Notify team",
            },
          },
          required: ["environment", "replicas", "notify"],
        },
      },
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        onMcpElicitationSubmit={onMcpElicitationSubmit}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: "Production" }));
    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("radio", { name: "Yes" }));
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    expect(onMcpElicitationSubmit).toHaveBeenCalledWith("message-mcp", "accept", {
      environment: "production",
      replicas: 3,
      notify: true,
    });
  });

  it("blocks MCP elicitation submission when a number is out of range", () => {
    const onMcpElicitationSubmit = vi.fn();
    const message: McpElicitationRequestMessage = {
      id: "message-mcp-range",
      type: "mcpElicitationRequest",
      author: "assistant",
      timestamp: "10:05",
      title: "Codex needs MCP input",
      detail: "MCP server deployment-helper requested additional structured input.",
      state: "pending",
      request: {
        threadId: "thread-1",
        turnId: "turn-1",
        serverName: "deployment-helper",
        mode: "form",
        message: "Confirm the deployment settings.",
        requestedSchema: {
          type: "object",
          properties: {
            replicas: {
              type: "integer",
              title: "Replicas",
              minimum: 2,
              maximum: 5,
            },
          },
          required: ["replicas"],
        },
      },
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        onMcpElicitationSubmit={onMcpElicitationSubmit}
      />,
    );

    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "1" } });
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    expect(onMcpElicitationSubmit).not.toHaveBeenCalled();
    expect(screen.getByText('"Replicas" must be at least 2.')).toBeInTheDocument();
  });

  it("blocks MCP elicitation submission when string and array constraints are violated", () => {
    const onMcpElicitationSubmit = vi.fn();
    const message: McpElicitationRequestMessage = {
      id: "message-mcp-constraints",
      type: "mcpElicitationRequest",
      author: "assistant",
      timestamp: "10:05",
      title: "Codex needs MCP input",
      detail: "MCP server deployment-helper requested additional structured input.",
      state: "pending",
      request: {
        threadId: "thread-1",
        turnId: "turn-1",
        serverName: "deployment-helper",
        mode: "form",
        message: "Confirm the deployment settings.",
        requestedSchema: {
          type: "object",
          properties: {
            ticket: {
              type: "string",
              title: "Ticket",
              minLength: 3,
            },
            reviewers: {
              type: "array",
              title: "Reviewers",
              items: {
                type: "string",
                anyOf: [
                  { const: "alice", title: "Alice" },
                  { const: "bob", title: "Bob" },
                ],
              },
              maxItems: 1,
            },
          },
          required: ["ticket", "reviewers"],
        },
      },
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        onMcpElicitationSubmit={onMcpElicitationSubmit}
      />,
    );

    fireEvent.change(screen.getByRole("textbox"), { target: { value: "ab" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "Alice" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Bob" }));
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    expect(onMcpElicitationSubmit).not.toHaveBeenCalled();
    expect(screen.getByText('"Ticket" must be at least 3 characters.')).toBeInTheDocument();

    fireEvent.change(screen.getByRole("textbox"), { target: { value: "abc-123" } });
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    expect(onMcpElicitationSubmit).not.toHaveBeenCalled();
    expect(screen.getByText('"Reviewers" must include at most 1 selections.')).toBeInTheDocument();
  });

  it("submits generic Codex app request JSON results", () => {
    const onCodexAppRequestSubmit = vi.fn();
    const message: CodexAppRequestMessage = {
      id: "message-codex-request",
      type: "codexAppRequest",
      author: "assistant",
      timestamp: "10:06",
      title: "Codex needs a tool result",
      detail: "Codex requested a result for `search_workspace`.",
      method: "item/tool/call",
      params: {
        toolName: "search_workspace",
        arguments: {
          pattern: "Codex",
        },
      },
      state: "pending",
    };

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        onCodexAppRequestSubmit={onCodexAppRequestSubmit}
      />,
    );

    fireEvent.change(screen.getByRole("textbox"), {
      target: { value: '{\n  "matches": ["docs/bugs.md"]\n}' },
    });
    fireEvent.click(screen.getByRole("button", { name: "Submit JSON result" }));

    expect(onCodexAppRequestSubmit).toHaveBeenCalledWith("message-codex-request", {
      matches: ["docs/bugs.md"],
    });
  });

  it("opens assistant file links through the source callback", () => {
    const message: TextMessage = {
      id: "message-5",
      type: "text",
      author: "assistant",
      timestamp: "10:04",
      text: "[experience.tex#L63](experience.tex#L63)",
    };
    const onOpenSourceLink = vi.fn();

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
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

  it("opens assistant absolute Windows file links through the source callback", () => {
    const message: TextMessage = {
      id: "message-5b",
      type: "text",
      author: "assistant",
      timestamp: "10:04",
      text: "[route_post_processing_service.dart:469](C:/github/Personal/fit_friends/lib/services/route_post_processing_service.dart#L469)",
    };
    const onOpenSourceLink = vi.fn();

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/github/Personal/TermAl"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "route_post_processing_service.dart:469" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "C:/github/Personal/fit_friends/lib/services/route_post_processing_service.dart",
      line: 469,
      openInNewTab: false,
    });
  });

  it("opens assistant absolute Linux file links through the source callback", () => {
    const message: TextMessage = {
      id: "message-5c",
      type: "text",
      author: "assistant",
      timestamp: "10:04",
      text: "[route_post_processing_service.dart:469](/home/grzeg/projects/fit_friends/lib/services/route_post_processing_service.dart#L469)",
    };
    const onOpenSourceLink = vi.fn();

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "route_post_processing_service.dart:469" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/home/grzeg/projects/fit_friends/lib/services/route_post_processing_service.dart",
      line: 469,
      openInNewTab: false,
    });
  });

  it("autolinks bare assistant file references through the source callback", () => {
    const message: TextMessage = {
      id: "message-6",
      type: "text",
      author: "assistant",
      timestamp: "10:05",
      text: "The Microsoft scope bullet needs more evidence in experience.tex#L63.",
    };
    const onOpenSourceLink = vi.fn();

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
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

  it("opens inline assistant file references through the source callback", () => {
    const message: TextMessage = {
      id: "message-7",
      type: "text",
      author: "assistant",
      timestamp: "10:06",
      text: "Text like `experience.tex.#L63` should stay clickable.",
    };
    const onOpenSourceLink = vi.fn();

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
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

  it("renders a live connection-retry notice with spinner and present-tense heading", () => {
    const message: TextMessage = {
      id: "message-retry-live",
      type: "text",
      author: "assistant",
      timestamp: "15:47:58",
      text: "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        isLatestAssistantMessage
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Reconnecting to continue this turn" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Attempt 2 of 5")).toBeInTheDocument();
    expect(container.querySelector(".connection-notice-spinner")).not.toBeNull();
    const card = container.querySelector(".connection-notice-card");
    expect(card?.classList.contains("connection-notice-card-resolved")).toBe(false);
    expect(card?.getAttribute("aria-live")).toBe("polite");
  });

  it("renders a resolved connection-retry notice without spinner when later assistant output exists", () => {
    const message: TextMessage = {
      id: "message-retry-resolved",
      type: "text",
      author: "assistant",
      timestamp: "15:47:58",
      text: "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        isLatestAssistantMessage={false}
      />,
    );

    expect(screen.getByRole("heading", { name: "Connection recovered" })).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Reconnecting to continue this turn" }),
    ).not.toBeInTheDocument();
    expect(container.querySelector(".connection-notice-spinner")).toBeNull();
    expect(
      container.querySelector(".connection-notice-card-resolved"),
    ).not.toBeNull();
    expect(
      container.querySelector(".connection-notice-card")?.getAttribute("aria-live"),
    ).toBe("off");
    // Past-tense detail copy references the attempt the retry recovered from.
    expect(
      screen.getByText(/the turn continued after attempt 2 of 5\.$/),
    ).toBeInTheDocument();
  });

  it("renders an explicit resolved connection-retry display state", () => {
    const message: TextMessage = {
      id: "message-retry-resolved-explicit",
      type: "text",
      author: "assistant",
      timestamp: "15:47:58",
      text: "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        connectionRetryDisplayState="resolved"
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Connection recovered" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Connection dropped briefly; the turn continued after attempt 2 of 5.",
      ),
    ).toBeInTheDocument();
    expect(container.querySelector(".connection-notice-spinner")).toBeNull();
    const card = container.querySelector(".connection-notice-card");
    expect(card?.getAttribute("aria-live")).toBe("off");
    expect(
      container.querySelector(".connection-notice-card-resolved"),
    ).not.toBeNull();
  });

  it("renders a superseded connection-retry notice without claiming recovery", () => {
    const message: TextMessage = {
      id: "message-retry-superseded",
      type: "text",
      author: "assistant",
      timestamp: "15:47:58",
      text: "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        connectionRetryDisplayState="superseded"
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Retry superseded" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Connection recovered" }),
    ).not.toBeInTheDocument();
    expect(container.querySelector(".connection-notice-spinner")).toBeNull();
    expect(
      screen.getByText("A newer reconnect attempt continued the turn."),
    ).toBeInTheDocument();
    const card = container.querySelector(".connection-notice-card");
    expect(card?.getAttribute("aria-live")).toBe("off");
    expect(
      container.querySelector(".connection-notice-card-settled"),
    ).not.toBeNull();
  });

  it("renders an inactive latest connection-retry notice without a live spinner", () => {
    const message: TextMessage = {
      id: "message-retry-inactive",
      type: "text",
      author: "assistant",
      timestamp: "15:47:58",
      text: "Connection dropped before the response finished. Retrying automatically.",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        connectionRetryDisplayState="inactive"
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Connection retry ended" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("The session is no longer running this turn."),
    ).toBeInTheDocument();
    expect(container.querySelector(".connection-notice-spinner")).toBeNull();
    expect(
      container
        .querySelector(".connection-notice-card")
        ?.getAttribute("aria-live"),
    ).toBe("off");
    expect(
      container.querySelector(".connection-notice-card-settled"),
    ).not.toBeNull();
  });

  it("renders an inactive connection-retry notice with the attempt chip", () => {
    const message: TextMessage = {
      id: "message-retry-inactive-attempt",
      type: "text",
      author: "assistant",
      timestamp: "15:47:58",
      text: "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5).",
    };

    const { container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        connectionRetryDisplayState="inactive"
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Connection retry ended" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Attempt 2 of 5")).toBeInTheDocument();
    expect(
      screen.getByText("The session is no longer running this turn."),
    ).toBeInTheDocument();
    expect(container.querySelector(".connection-notice-spinner")).toBeNull();
    expect(
      container.querySelector(".connection-notice-card-settled"),
    ).not.toBeNull();
  });

  it("hides the attempt chip and uses the generic copy when the retry notice omits the attempt suffix", () => {
    // Legacy / fallback backend path: `summarize_retryable_connectivity_error`
    // emits `"Connection dropped before the response finished. Retrying automatically."`
    // (no parenthesized attempt counter), so `parseConnectionRetryNotice`
    // returns `attemptLabel: null`. Both the chip and the past-tense
    // attempt-specific detail must be absent on the resolved render, and
    // likewise the attempt chip on the live render.
    const message: TextMessage = {
      id: "message-retry-no-attempt",
      type: "text",
      author: "assistant",
      timestamp: "15:47:58",
      text: "Connection dropped before the response finished. Retrying automatically.",
    };

    const { rerender, container } = render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        isLatestAssistantMessage
      />,
    );

    // Live render: spinner + present-tense heading, but no attempt chip.
    expect(
      screen.getByRole("heading", { name: "Reconnecting to continue this turn" }),
    ).toBeInTheDocument();
    expect(container.querySelector(".connection-notice-spinner")).not.toBeNull();
    expect(screen.queryByText(/^Attempt \d+ of \d+$/)).toBeNull();

    rerender(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onUserInputSubmit={vi.fn()}
        isLatestAssistantMessage={false}
      />,
    );

    // Resolved render: generic past-tense detail, no attempt chip.
    expect(screen.getByRole("heading", { name: "Connection recovered" })).toBeInTheDocument();
    expect(container.querySelector(".connection-notice-spinner")).toBeNull();
    expect(screen.queryByText(/^Attempt \d+ of \d+$/)).toBeNull();
    expect(
      screen.getByText(
        "Connection dropped briefly; the turn continued after an automatic retry.",
      ),
    ).toBeInTheDocument();
  });
});
