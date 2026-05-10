import { describe, expect, it } from "vitest";

import { ApiRequestError } from "./api";
import {
  resumeWaitFailurePacket,
  spawnDelegationFailurePacket,
  spawnDelegationValidationFailurePacket,
} from "./delegation-error-packets";

const PARENT_SESSION_ID = "parent-1";

function spawnFailureMessage(error: unknown) {
  return spawnDelegationFailurePacket(error, {
    parentSessionId: PARENT_SESSION_ID,
  }).message;
}

function resumeWaitFailureMessage(error: unknown) {
  return resumeWaitFailurePacket(error, {
    parentSessionId: PARENT_SESSION_ID,
  }).message;
}

describe("delegation error packets", () => {
  it.each([
    ["backend unavailable", "The TermAl backend is unavailable."],
    ["session missing", "session not found"],
    ["delegation missing", "delegation not found"],
    ["empty ids", "delegation wait ids cannot be empty"],
    [
      "requires id",
      "delegation wait requires at least one delegation id",
    ],
  ])("passes through safe resume-wait message: %s", (_caseName, message) => {
    expect(
      resumeWaitFailureMessage(
        new ApiRequestError("request-failed", message, { status: 400 }),
      ),
    ).toBe(message);
  });

  it.each([
    ["request status", "Request failed with status 500."],
    [
      "id cap",
      "delegation wait accepts at most 10 delegation ids",
    ],
    [
      "title cap",
      "delegation wait title must be at most 200 characters",
    ],
    [
      "wrong parent",
      "delegation `delegation-1` does not belong to parent session `session-a`",
    ],
  ])("passes through safe resume-wait pattern: %s", (_caseName, message) => {
    expect(
      resumeWaitFailureMessage(
        new ApiRequestError("request-failed", message, { status: 400 }),
      ),
    ).toBe(message);
  });

  it.each([
    [
      "status extra detail",
      "Request failed with status 500 at C:/secret.log.",
    ],
    [
      "id cap path",
      "delegation wait accepts at most 10 delegation ids from C:/secret.log",
    ],
    [
      "title cap missing number",
      "delegation wait title must be at most many characters",
    ],
    [
      "wrong parent extra",
      "delegation `delegation-1` does not belong to parent session `session-a` because token=secret",
    ],
  ])("redacts unsafe resume-wait near-miss: %s", (_caseName, message) => {
    expect(
      resumeWaitFailureMessage(
        new ApiRequestError("request-failed", message, { status: 500 }),
      ),
    ).toBe("Delegation resume wait scheduling failed.");
  });

  it("passes through backend-unavailable resume-wait route diagnostics for the requested parent session", () => {
    const message =
      "The running backend does not expose /api/sessions/parent-1/delegation-waits (HTTP 404). Restart TermAl so the latest API routes are loaded.";

    expect(
      resumeWaitFailureMessage(
        new ApiRequestError("backend-unavailable", message, {
          status: 404,
          restartRequired: true,
        }),
      ),
    ).toBe(message);
  });

  it("redacts backend-unavailable resume-wait route diagnostics for other parent sessions", () => {
    const message =
      "The running backend does not expose /api/sessions/token=secret/delegation-waits (HTTP 404). Restart TermAl so the latest API routes are loaded.";

    expect(
      resumeWaitFailureMessage(
        new ApiRequestError("backend-unavailable", message, {
          status: 404,
          restartRequired: true,
        }),
      ),
    ).toBe("Delegation resume wait scheduling failed.");
  });

  it("redacts non-Error resume-wait failures", () => {
    expect(resumeWaitFailurePacket("token=secret", {
      parentSessionId: PARENT_SESSION_ID,
    })).toMatchObject({
      kind: "resume-wait-failed",
      name: "Error",
      message: "Delegation resume wait scheduling failed.",
      apiErrorKind: null,
      status: null,
      restartRequired: null,
    });
  });

  it.each([
    ["backend unavailable", "The TermAl backend is unavailable."],
    ["parent session id", "parent session id is required"],
    ["session missing", "session not found"],
    ["empty prompt", "delegation prompt cannot be empty"],
    ["worker mode", "worker delegations are not implemented in Phase 1"],
    [
      "write policy",
      "only readOnly delegation write policy is implemented in Phase 1",
    ],
    [
      "remote session",
      "delegations for remote-backed sessions are not implemented in Phase 1",
    ],
    [
      "remote project",
      "delegations for remote-backed projects are not implemented in Phase 1",
    ],
  ])("passes through safe spawn message: %s", (_caseName, message) => {
    expect(
      spawnFailureMessage(
        new ApiRequestError("request-failed", message, { status: 400 }),
      ),
    ).toBe(message);
  });

  it.each([
    ["request status", "Request failed with status 500."],
    ["prompt bytes", "delegation prompt must be at most 65536 bytes"],
    ["title chars", "delegation title must be at most 200 characters"],
    ["model chars", "delegation model must be at most 200 characters"],
    ["active limit", "parent session already has 4 active delegations"],
    ["depth limit", "delegation nesting depth is limited to 4"],
    [
      "drive-relative cwd",
      "delegation cwd cannot be a drive-relative Windows path",
    ],
    ["UNC cwd", "delegation cwd cannot be a UNC path"],
    [
      "device namespace cwd",
      "delegation cwd cannot be a Windows device namespace path",
    ],
    ["unknown project", "unknown project `project-1`"],
    [
      "project boundary cwd",
      "delegation cwd `C:\\repo\\outside` must stay inside project `TermAl`",
    ],
  ])("passes through safe spawn pattern: %s", (_caseName, message) => {
    expect(
      spawnFailureMessage(
        new ApiRequestError("request-failed", message, { status: 400 }),
      ),
    ).toBe(message);
  });

  it.each([
    ["status extra detail", "Request failed with status 500 at C:/secret.log."],
    [
      "prompt bytes path",
      "delegation prompt must be at most 65536 bytes in C:/secret.log",
    ],
    [
      "title missing number",
      "delegation title must be at most many characters",
    ],
    [
      "model missing number",
      "delegation model must be at most many characters",
    ],
    [
      "active limit path",
      "parent session already has 4 active delegations in C:/secret.log",
    ],
    [
      "depth limit path",
      "delegation nesting depth is limited to 4 in C:/secret.log",
    ],
    ["cwd unknown shape", "delegation cwd cannot be /home/secret/path"],
    [
      "unknown project path",
      "unknown project `C:/secret/project` extra detail",
    ],
    [
      "project boundary extra",
      "delegation cwd `C:\\repo\\outside` must stay inside project `TermAl` because token=secret",
    ],
  ])("redacts unsafe spawn near-miss: %s", (_caseName, message) => {
    expect(
      spawnFailureMessage(
        new ApiRequestError("request-failed", message, { status: 500 }),
      ),
    ).toBe("Spawn delegation failed.");
  });

  it("passes through backend-unavailable route diagnostics for the requested parent session", () => {
    const message =
      "The running backend does not expose /api/sessions/parent-1/delegations (HTTP 404). Restart TermAl so the latest API routes are loaded.";

    expect(
      spawnFailureMessage(
        new ApiRequestError("backend-unavailable", message, {
          status: 404,
          restartRequired: true,
        }),
      ),
    ).toBe(message);
  });

  it("redacts backend-unavailable route diagnostics for other parent sessions", () => {
    const message =
      "The running backend does not expose /api/sessions/token=secret/delegations (HTTP 404). Restart TermAl so the latest API routes are loaded.";

    expect(
      spawnFailureMessage(
        new ApiRequestError("backend-unavailable", message, {
          status: 404,
          restartRequired: true,
        }),
      ),
    ).toBe("Spawn delegation failed.");
  });

  it.each([
    ["parent type", new TypeError("parent session id must be a string")],
    ["parent empty", new RangeError("parent session id must be non-empty")],
    [
      "parent unsafe",
      new RangeError(
        "parent session id must not contain /, ?, #, or control characters",
      ),
    ],
    ["prompt type", new TypeError("prompt must be a string")],
    ["prompt empty", new RangeError("prompt must be non-empty")],
    ["title null", new TypeError("title must be omitted instead of null")],
    ["cwd null", new TypeError("cwd must be omitted instead of null")],
    ["agent null", new TypeError("agent must be omitted instead of null")],
    ["model null", new TypeError("model must be omitted instead of null")],
    ["mode null", new TypeError("mode must be omitted instead of null")],
    [
      "writePolicy null",
      new TypeError("writePolicy must be omitted instead of null"),
    ],
    [
      "batch type",
      new TypeError("spawn_reviewer_batch requests must be an array"),
    ],
    [
      "batch empty",
      new RangeError("spawn_reviewer_batch requires at least one reviewer"),
    ],
    [
      "prompt bytes",
      new RangeError("prompt must be no larger than 65536 bytes"),
    ],
    [
      "title chars",
      new RangeError("title must be no longer than 200 characters"),
    ],
    [
      "model chars",
      new RangeError("model must be no longer than 200 characters"),
    ],
    [
      "batch max",
      new RangeError("spawn_reviewer_batch accepts at most 4 reviewers"),
    ],
    ["reviewer shape", new TypeError("reviewer request 2 must be an object")],
  ])("passes through safe validation message: %s", (_caseName, error) => {
    const packet = spawnDelegationValidationFailurePacket(error);

    expect(packet).toEqual({
      kind: "validation-failed",
      name: error.name,
      message: error.message,
    });
    expect(packet).not.toHaveProperty("apiErrorKind");
    expect(packet).not.toHaveProperty("status");
    expect(packet).not.toHaveProperty("restartRequired");
  });

  it("redacts unexpected validation exceptions", () => {
    expect(
      spawnDelegationValidationFailurePacket(
        new TypeError("token=secret C:/internal/backend.log"),
      ),
    ).toMatchObject({
      kind: "validation-failed",
      name: "TypeError",
      message: "Invalid delegation request.",
    });
  });

  it("redacts unexpected validation error names", () => {
    const error = new RangeError("prompt must be non-empty");
    error.name = "token=secret";

    expect(spawnDelegationValidationFailurePacket(error)).toEqual({
      kind: "validation-failed",
      name: "Error",
      message: "prompt must be non-empty",
    });
  });
});
