import type { Message, Session } from "./types";

export const initialSessions: Session[] = [
  {
    id: "backend-claude",
    name: "Backend",
    emoji: "##",
    agent: "Claude",
    workdir: "/projects/api",
    model: "claude-opus-4-6",
    status: "active",
    preview: "The issue was the token expiry check using milliseconds.",
    messages: [
      text("m1", "you", "10:42", "Fix the auth middleware. Refresh tokens are throwing 401s."),
      thinking("m2", "assistant", "10:42", "Claude is tracing the auth path", [
        "Reading src/middleware/auth.ts",
        "Comparing refresh token expiry math",
        "Queueing a narrow test run",
      ]),
      command(
        "m3",
        "assistant",
        "10:43",
        "npm test -- auth.middleware",
        "PASS auth.middleware.spec.ts\n24 tests passing\n",
        "success",
      ),
      diff(
        "m4",
        "assistant",
        "10:44",
        "src/middleware/auth.ts",
        "+12 -4 lines",
        "@@ -42,8 +42,12 @@\n- const isExpired = token.exp < Date.now()\n+ const isExpired = token.exp < Date.now() / 1000\n+ // token.exp is in seconds; Date.now() is milliseconds\n  if (isExpired) {\n    throw new UnauthorizedError()\n  }",
        "edit",
      ),
      text(
        "m5",
        "assistant",
        "10:44",
        "The issue was a seconds-vs-milliseconds comparison in the refresh token check. I updated the expiry math and the targeted auth tests pass.",
      ),
    ],
  },
  {
    id: "frontend-codex",
    name: "Frontend",
    emoji: "@@",
    agent: "Codex",
    workdir: "/projects/web",
    model: "gpt-5.4",
    status: "approval",
    preview: "Codex wants approval to install a UI dependency.",
    messages: [
      text("m6", "you", "11:08", "Add a command palette for session switching."),
      markdown(
        "m7",
        "assistant",
        "11:09",
        "Plan",
        "## Proposed update\n\n- Add a searchable command palette overlay\n- Reuse the existing session list data model\n- Bind `cmd+k` to open the palette\n",
      ),
      approval(
        "m8",
        "assistant",
        "11:09",
        "Codex wants to execute a command",
        "npm install cmdk",
        "Needed for the command palette interaction layer.",
      ),
    ],
  },
  {
    id: "docs-claude",
    name: "Product Notes",
    emoji: "**",
    agent: "Claude",
    workdir: "/projects/termal",
    model: "claude-sonnet-4-5",
    status: "idle",
    preview: "Phase 1 should stay local-only until resume and approvals are stable.",
    messages: [
      markdown(
        "m9",
        "assistant",
        "09:15",
        "Spec summary",
        "# Phase 1\n\n- Local desktop only\n- Multiple named sessions\n- Structured cards for diffs, commands, markdown, approvals\n\n## Constraint\n\nDo not start remote relay work before local resume and reliability feel solid.\n",
      ),
    ],
  },
  {
    id: "migrations-codex",
    name: "DB Migrations",
    emoji: "!!",
    agent: "Codex",
    workdir: "/projects/infra",
    model: "gpt-5.4-mini",
    status: "error",
    preview: "Migration failed: database lock timeout while applying patch.",
    messages: [
      text(
        "m10",
        "assistant",
        "08:32",
        "Migration dry-run failed because the target database was locked by another process.",
      ),
      command(
        "m11",
        "assistant",
        "08:32",
        "sqlx migrate run",
        "error: database is locked\n",
        "error",
      ),
    ],
  },
];

export function buildPrototypeReply(sessionName: string, prompt: string): Message[] {
  return [
    thinking(`thinking-${cryptoId()}`, "assistant", nowStamp(), `${sessionName} is shaping a reply`, [
      "Streaming is mocked in the UI shell",
      "Rust session wiring will replace this local response",
    ]),
    text(
      `reply-${cryptoId()}`,
      "assistant",
      nowStamp(),
      `UI prototype received: "${prompt}". Backend streaming for this session will plug into the same card layout.`,
    ),
  ];
}

function nowStamp(): string {
  const now = new Date();
  return now.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function cryptoId(): string {
  return Math.random().toString(36).slice(2, 10);
}

function text(id: string, author: "you" | "assistant", timestamp: string, value: string) {
  return {
    id,
    type: "text" as const,
    author,
    timestamp,
    text: value,
  };
}

function thinking(
  id: string,
  author: "you" | "assistant",
  timestamp: string,
  title: string,
  lines: string[],
) {
  return {
    id,
    type: "thinking" as const,
    author,
    timestamp,
    title,
    lines,
  };
}

function command(
  id: string,
  author: "you" | "assistant",
  timestamp: string,
  shellCommand: string,
  output: string,
  status: "running" | "success" | "error",
) {
  return {
    id,
    type: "command" as const,
    author,
    timestamp,
    command: shellCommand,
    output,
    status,
  };
}

function diff(
  id: string,
  author: "you" | "assistant",
  timestamp: string,
  filePath: string,
  summary: string,
  patch: string,
  changeType: "edit" | "create",
) {
  return {
    id,
    type: "diff" as const,
    author,
    timestamp,
    filePath,
    summary,
    diff: patch,
    changeType,
  };
}

function markdown(
  id: string,
  author: "you" | "assistant",
  timestamp: string,
  title: string,
  value: string,
) {
  return {
    id,
    type: "markdown" as const,
    author,
    timestamp,
    title,
    markdown: value,
  };
}

function approval(
  id: string,
  author: "you" | "assistant",
  timestamp: string,
  title: string,
  shellCommand: string,
  detail: string,
) {
  return {
    id,
    type: "approval" as const,
    author,
    timestamp,
    title,
    command: shellCommand,
    detail,
    decision: "pending" as const,
  };
}
