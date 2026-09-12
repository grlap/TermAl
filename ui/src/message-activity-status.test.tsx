import { cleanup, render } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { MessageCard } from "./message-cards";
import type { Message, UserInputRequestMessage } from "./types";

afterEach(cleanup);

const meta = { id: "operation", author: "assistant", timestamp: "10:00" } as const;
const input: UserInputRequestMessage = {
  ...meta, type: "userInputRequest", title: "Question", detail: "Pick one",
  state: "pending", declinable: false, questions: [],
};
function card(message: Message, enabled = true, submitting = false) {
  return <MessageCard message={message} onApprovalDecision={() => {}}
    onUserInputSubmit={async () => {}} approvalActionsEnabled={enabled}
    userInputActionsEnabled={enabled} userInputSubmissionPending={submitting} />;
}

it("keeps command status text without duplicating the trailing activity squares", () => {
  const command = { ...meta, type: "command", command: "pwd", output: "", status: "running" } as const;
  const view = render(card(command));
  const status = view.container.querySelector(".message-activity-status")!;
  expect(status).toHaveAttribute("data-activity", "running");
  expect(status).toHaveTextContent("running");
  expect(status.querySelector(".message-activity-squares")).toBeNull();
  for (const state of ["success", "error"] as const) {
    view.rerender(card({ ...command, status: state }));
    expect(status).toHaveAttribute("data-activity", "inactive");
    expect(status).toHaveTextContent(state);
    expect(status.querySelector(".message-activity-squares")).toBeNull();
  }
});

it("uses each tool/subagent's own state in a mixed parallel card", () => {
  const states = ["initializing", "running", "completed", "error"] as const;
  const view = render(card({ ...meta, type: "parallelAgents", agents: states.map((status, i) => ({
    id: String(i), source: "tool", title: `Tool ${i}`, status,
  })) }));
  const markers = view.container.querySelectorAll(".message-activity-status");
  expect(Array.from(markers, (marker) => marker.getAttribute("data-activity")))
    .toEqual(["running", "running", "inactive", "inactive"]);
  expect(view.container.querySelector(".message-activity-squares")).toBeNull();
});

it("distinguishes input waiting, queued, sending and completed states", () => {
  const view = render(card(input));
  const status = view.container.querySelector(".message-activity-status")!;
  expect(status).toHaveAttribute("data-activity", "waiting");
  expect(status).toHaveTextContent("Waiting for you");
  view.rerender(card(input, false));
  expect(status).toHaveAttribute("data-activity", "inactive");
  expect(status).toHaveTextContent("Queued");
  view.rerender(card(input, true, true));
  expect(status).toHaveAttribute("data-activity", "running");
  expect(status).toHaveTextContent("Sending");
  expect(status.querySelector(".message-activity-squares")).toBeNull();
  view.rerender(card({ ...input, state: "submitted" }, true, true));
  expect(status).toHaveAttribute("data-activity", "inactive");
  expect(status).toHaveTextContent("submitted");
});

it.each<Message>([
  { ...meta, type: "approval", title: "Approval", command: "pwd", detail: "", decision: "pending" },
  { ...meta, type: "codexAppRequest", title: "Request", detail: "", method: "custom/request", params: {}, state: "pending" },
  { ...meta, type: "mcpElicitationRequest", title: "MCP", detail: "", state: "pending",
    request: { threadId: "thread", serverName: "test", mode: "url", message: "Sign in", url: "https://example.com", elicitationId: "request" } },
])("shows a static waiting marker for pending $type", (message) => {
  const view = render(card(message));
  expect(view.container.querySelector(".message-activity-status"))
    .toHaveAttribute("data-activity", "waiting");
  view.rerender(card(message.type === "approval"
    ? { ...message, decision: "accepted" }
    : { ...message, state: "submitted" } as Message));
  expect(view.container.querySelector(".message-activity-status"))
    .toHaveAttribute("data-activity", "inactive");
});
