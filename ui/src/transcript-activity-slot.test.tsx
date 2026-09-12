// Owns whole-turn labels, live-tail boundaries, accessibility and stable slot
// reservation tests. JSDOM checks CSS contracts, not browser scroll geometry.
import { cleanup, render } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { TranscriptActivitySlot, transcriptActivityLabel } from "./transcript-activity-slot";
import { isSessionAtLiveTail } from "./session-live-tail";
import type { CommandMessage, Message, Session } from "./types";

const prompt: Message = { id: "prompt", author: "you", type: "text", timestamp: "10:00", text: "Work" };
const command: CommandMessage = { id: "command", author: "assistant", type: "command", timestamp: "10:01",
  command: "pwd", output: "", status: "running" };
const answer: Message = { id: "answer", author: "assistant", type: "text", timestamp: "10:02", text: "Answer" };
const pendingPrompts = [{ id: "next", timestamp: "10:03", text: "Next task" }];
function session(messages: Message[], overrides: Partial<Session> = {}): Session {
  return { id: "session", name: "Session", emoji: "", agent: "Codex", workdir: "/repo", model: "test",
    status: "active", preview: "", messages, ...overrides };
}
afterEach(cleanup);

it("matches the strip during send, stop and delegation-wait handoffs", () => {
  const idle = session([], { status: "idle" });
  const view = render(<TranscriptActivitySlot session={idle} isSending />);
  const slot = view.container.firstElementChild;
  expect(slot).toHaveTextContent("Agent is working");
  view.rerender(<TranscriptActivitySlot session={idle} isStopping />);
  expect(slot).toHaveTextContent("Agent is stopping");
  view.rerender(<TranscriptActivitySlot session={idle} delegationWaitPrompt="Review pending" />);
  expect(slot).toHaveTextContent("Agent is working");
  view.rerender(<TranscriptActivitySlot session={idle} />);
  expect(view.container.firstElementChild).toBe(slot);
  expect(slot).toBeEmptyDOMElement();
});

it("keeps Agent is working visible during a running command", () => {
  const view = render(<TranscriptActivitySlot session={session([prompt, command])} />);
  expect(view.container).toHaveTextContent("Agent is working");
});

it("shows activity throughout the turn regardless of streaming or resident turn evidence", () => {
  for (const messages of [[], [prompt], [answer], [prompt, answer], [command, answer, prompt],
    [prompt, command], [prompt, { ...command, status: "success" } as CommandMessage],
    [prompt, { ...command, status: "error" } as CommandMessage]]) {
    expect(transcriptActivityLabel(session(messages))).toBe("Agent is working");
  }
});

it.each(["initializing", "running", "completed", "error"] as const)(
  "keeps the line visible during %s parallel work", (status) => {
    const parallel: Message = { id: "parallel", author: "assistant", timestamp: "10:01", type: "parallelAgents",
      agents: [{ id: "child", source: "tool", title: "Tool", status }] };
    expect(transcriptActivityLabel(session([prompt, parallel]))).toBe("Agent is working");
  },
);

it.each(["Codex", "Claude", "OpenCode", "Cursor", "Gemini"] as const)(
  "uses the same working/stopping labels for %s", (agent) => {
    expect(transcriptActivityLabel(session([answer], { agent }))).toBe("Agent is working");
    expect(transcriptActivityLabel(session([answer], { agent, status: "stopping" }))).toBe("Agent is stopping");
  },
);

it("shows queue handoff but not a paused idle queue", () => {
  expect(transcriptActivityLabel(session([], { status: "idle", pendingPrompts }))).toBe("Agent is working");
  expect(transcriptActivityLabel(session([], { status: "idle", pendingPrompts, queuePaused: true }))).toBeNull();
  expect(transcriptActivityLabel(session([], { pendingPrompts, queuePaused: true }))).toBe("Agent is working");
});

it.each(["idle", "approval", "error"] as const)("is empty for %s without active work", (status) => {
  expect(transcriptActivityLabel(session([prompt], { status }))).toBeNull();
  if (status !== "idle") {
    expect(transcriptActivityLabel(session([prompt], { status, pendingPrompts }))).toBeNull();
  }
});

it("only treats windows reaching the live tail as current", () => {
  expect(isSessionAtLiveTail(session([prompt]))).toBe(true);
  expect(isSessionAtLiveTail(session([prompt], { messageCount: 1 }))).toBe(true);
  expect(isSessionAtLiveTail(session([prompt], { hasNewerHistory: true }))).toBe(false);
  expect(isSessionAtLiveTail(session([prompt], { hasNewerHistory: false, messageCount: 3 }))).toBe(false);
  expect(isSessionAtLiveTail(session([prompt], { messageCount: 3, messageStartIndex: 2 }))).toBe(true);
  expect(isSessionAtLiveTail(session([prompt], { hasNewerHistory: true, messageCount: 3, messageStartIndex: 2 }))).toBe(false);
});

it("keeps historical active, stopping and queued windows empty", () => {
  for (const status of ["active", "stopping", "idle"] as const) {
    expect(transcriptActivityLabel(session([prompt], { status, pendingPrompts, hasNewerHistory: true }))).toBeNull();
    expect(transcriptActivityLabel(session([prompt], { status, pendingPrompts, messageCount: 3 }))).toBeNull();
  }
  expect(transcriptActivityLabel(null)).toBeNull();
});

it("leaves session announcements to the activity strip", () => {
  const view = render(<TranscriptActivitySlot session={session([prompt])} />);
  const slot = view.container.firstElementChild!;
  expect(slot).toHaveTextContent("Agent is working");
  expect(slot).toHaveAttribute("aria-hidden", "true");
  expect(slot).not.toHaveAttribute("aria-live");
  expect(slot).not.toHaveAttribute("role");
  expect(view.queryByRole("status")).toBeNull();
});

it("keeps the same one-line CSS box and DOM node through every activity state", async () => {
  // CSS imports are stubbed by Vitest. Resolve the production file relative to
  // this module, not the runner's working directory. Pass a URL string across
  // the JSDOM/Node realm boundary.
  const nodeFsModule = "node:fs";
  const nodeUrlModule = "node:url";
  const { readFileSync } = await import(nodeFsModule) as {
    readFileSync: (path: string, encoding: "utf8") => string;
  };
  const { fileURLToPath } = await import(nodeUrlModule) as {
    fileURLToPath: (url: string) => string;
  };
  // Keep Vite's browser-asset rewrite away from this filesystem lookup.
  const moduleUrl = import.meta.url;
  const css = readFileSync(fileURLToPath(new URL("./transcript-activity-slot.css", moduleUrl).href), "utf8");
  const style = document.createElement("style");
  expect(css, "test must load the actual slot stylesheet").toContain("height: 1.5em");
  style.textContent = css;
  document.head.append(style);
  try {
    const view = render(<TranscriptActivitySlot session={session([prompt])} />);
    const slot = view.container.firstElementChild as HTMLElement;
    const squares = slot.querySelector(".message-activity-squares")!;
    expect(squares.children).toHaveLength(3);
    expect(getComputedStyle(squares.children[0]).animation).toContain("transcript-activity-lift 1.1s");
    expect(getComputedStyle(squares.children[1]).animationDelay).toBe("0.16s");
    expect(getComputedStyle(squares.children[2]).animationDelay).toBe("0.32s");
    expect(getComputedStyle(slot.querySelector(".message-activity-label")!).animation)
      .toContain("transcript-activity-breathe 2.2s");
    expect(getComputedStyle(slot.querySelector(".message-activity-status")!).textTransform).toBe("none");
    for (const value of [session([prompt]), session([prompt, answer]), session([prompt, command]),
      session([], { status: "stopping" }), session([], { status: "idle", pendingPrompts }),
      session([], { status: "idle", pendingPrompts, queuePaused: true }),
      session([], { status: "approval" }), session([], { status: "error" }),
      session([prompt], { hasNewerHistory: true }), session([], { status: "idle" }), null]) {
      view.rerender(<TranscriptActivitySlot session={value} />);
      expect(view.container.firstElementChild).toBe(slot);
      const computed = getComputedStyle(slot);
      expect(computed.height).toBe("1.5em");
      expect(computed.minHeight).toBe("1.5em");
      expect(computed.maxHeight).toBe("1.5em");
      expect(computed.overflow).toBe("hidden");
    }
    expect(slot).toBeEmptyDOMElement();
    expect(css).toContain("@keyframes transcript-activity-lift");
    expect(css).toContain("transform: translateY(-0.18em)");
    const reducedMotion = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
    expect(reducedMotion).toContain(".message-activity-label");
    expect(reducedMotion).toContain(".message-activity-squares > span");
    expect(reducedMotion).toContain("animation: none");
    expect(reducedMotion).toContain("transform: none");
    expect(reducedMotion).toContain("opacity: 0.7");
  } finally {
    style.remove();
  }
});
