import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MessageCard } from "./App";
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
});
