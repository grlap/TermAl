import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ApiRequestError,
  createOrchestratorInstance,
  deleteWorkspaceLayout,
  fetchWorkspaceLayout,
  fetchState,
  isBackendUnavailableError,
  runTerminalCommand,
  runTerminalCommandStream,
  saveFile,
  TERMINAL_SSE_BUFFER_MAX_CHARS,
} from "./api";

describe("createOrchestratorInstance", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("omits null project ids so the backend can fall back to the template project", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ orchestrator: {}, state: {} }), {
        status: 201,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await createOrchestratorInstance("template-run", null);

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/orchestrators",
      expect.objectContaining({
        method: "POST",
      }),
    );
    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({ templateId: "template-run" });
    expect(Object.prototype.hasOwnProperty.call(parsedBody, "projectId")).toBe(false);
  });

  it("includes inline template drafts when provided", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ orchestrator: {}, state: {} }), {
        status: 201,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await createOrchestratorInstance("template-run", "project-a", {
      name: "Run Flow",
      description: "Remote launch draft.",
      projectId: "project-a",
      sessions: [],
      transitions: [],
    });

    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({
      templateId: "template-run",
      projectId: "project-a",
      template: {
        name: "Run Flow",
        description: "Remote launch draft.",
        projectId: "project-a",
        sessions: [],
        transitions: [],
      },
    });
  });
});

describe("saveFile", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("omits empty session ids from the request body", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          path: "/repo/src/app.ts",
          content: "updated",
        }),
        {
          status: 200,
          headers: {
            "Content-Type": "application/json",
          },
        },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    await saveFile("/repo/src/app.ts", "updated", {
      sessionId: "",
      baseHash: "sha256:base",
    });

    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({
      path: "/repo/src/app.ts",
      content: "updated",
      baseHash: "sha256:base",
    });
    expect(Object.prototype.hasOwnProperty.call(parsedBody, "sessionId")).toBe(false);
  });

  it("omits empty base hashes from the request body", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          path: "/repo/src/app.ts",
          content: "updated",
        }),
        {
          status: 200,
          headers: {
            "Content-Type": "application/json",
          },
        },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    await saveFile("/repo/src/app.ts", "updated", {
      baseHash: " ",
    });

    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({
      path: "/repo/src/app.ts",
      content: "updated",
    });
  });
});

describe("runTerminalCommand", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("serializes the terminal command request and maps the backend response", async () => {
    const responseBody = {
      command: "npm test",
      durationMs: 42,
      exitCode: 7,
      outputTruncated: true,
      shell: "PowerShell",
      stderr: "warn\n",
      stdout: "ok\n",
      success: false,
      timedOut: false,
      workdir: "C:/repo",
    };
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(responseBody), {
        status: 200,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      runTerminalCommand({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).resolves.toEqual(responseBody);

    // Structurally compare the serialized body rather than coupling to
    // insertion order via `JSON.stringify(...)` string equality: a
    // refactor that reorders fields in `runTerminalCommand`'s call site
    // (or inserts an object spread) is semantically unchanged and should
    // not fail this test.
    const init = fetchMock.mock.calls[0]?.[1] as RequestInit | undefined;
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/terminal/run",
      expect.objectContaining({
        method: "POST",
        headers: expect.objectContaining({
          "Content-Type": "application/json",
        }),
      }),
    );
    expect(JSON.parse(init?.body as string)).toEqual({
      command: "npm test",
      projectId: "project-a",
      sessionId: null,
      workdir: "C:/repo",
    });
  });

  it("maps a 429 backend response to ApiRequestError with status 429", async () => {
    // Five assertions below: instanceof, status, kind, and two message
    // substrings. `.kind === "request-failed"` is the discriminator that
    // `isBackendUnavailableError` branches on — pinning it ensures a
    // regression that miscategorizes 429 as `"backend-unavailable"` would
    // fail this test instead of silently routing user-retryable rate
    // limits through the "backend is down" UX.
    expect.assertions(5);
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          error: "too many local terminal commands are already running; limit is 4",
        }),
        {
          status: 429,
          headers: {
            "Content-Type": "application/json",
          },
        },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    try {
      await runTerminalCommand({
        command: "echo blocked",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      });
      throw new Error("Expected runTerminalCommand to reject on 429");
    } catch (error) {
      // Pin the precondition that `formatTerminalCommandError`'s
      // rate-limit branch in `TerminalPanel.tsx` depends on: a fetch
      // rejection on 429 must surface as `ApiRequestError` with
      // `status === 429`, `kind === "request-failed"`, and the backend
      // message preserved. The rate-limit suffix text itself
      // (`(rate limit - try again in a moment)`) is pinned by a
      // separate TerminalPanel integration test; this test covers only
      // the fetch → ApiRequestError mapping, not the suffix rendering.
      expect(error).toBeInstanceOf(ApiRequestError);
      expect((error as ApiRequestError).status).toBe(429);
      expect((error as ApiRequestError).kind).toBe("request-failed");
      expect((error as Error).message).toContain(
        "too many local terminal commands",
      );
      expect((error as Error).message).toContain("limit is 4");
    }
  });
});

describe("runTerminalCommandStream", () => {
  const originalFetch = globalThis.fetch;
  const terminalResponseBody = {
    command: "npm test",
    durationMs: 42,
    exitCode: 0,
    outputTruncated: false,
    shell: "PowerShell",
    stderr: "",
    stdout: "building...\ndone\n",
    success: true,
    timedOut: false,
    workdir: "C:/repo",
  };

  function stubTerminalStream(chunks: string[]) {
    const encoder = new TextEncoder();
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        for (const chunk of chunks) {
          controller.enqueue(encoder.encode(chunk));
        }
        controller.close();
      },
    });
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(stream, {
        status: 200,
        headers: {
          "Content-Type": "text/event-stream",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    return fetchMock;
  }

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("streams output events and resolves with the final terminal response", async () => {
    const fetchMock = stubTerminalStream([
      'event: output\ndata: {"stream":"stdout","text":"building...\\n"}\n\n',
      `event: complete\ndata: ${JSON.stringify(terminalResponseBody)}\n\n`,
    ]);
    const outputEvents: Array<{ stream: string; text: string }> = [];

    await expect(
      runTerminalCommandStream(
        {
          command: "npm test",
          projectId: "project-a",
          sessionId: null,
          workdir: "C:/repo",
        },
        { onOutput: (event) => outputEvents.push(event) },
      ),
    ).resolves.toEqual(terminalResponseBody);

    expect(outputEvents).toEqual([
      { stream: "stdout", text: "building...\n" },
    ]);
    const init = fetchMock.mock.calls[0]?.[1] as RequestInit | undefined;
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/terminal/run/stream",
      expect.objectContaining({
        method: "POST",
        headers: expect.objectContaining({
          "Content-Type": "application/json",
        }),
      }),
    );
    expect(JSON.parse(init?.body as string)).toEqual({
      command: "npm test",
      projectId: "project-a",
      sessionId: null,
      workdir: "C:/repo",
    });
  });

  it("buffers split SSE frames and CRLF delimiters before parsing", async () => {
    stubTerminalStream([
      "event: output\r\n",
      'data: {"stream":"stdout","text":"build',
      'ing...\\n"}\r\n\r\n',
      `event: complete\r\ndata: ${JSON.stringify(terminalResponseBody)}\r\n\r\n`,
    ]);
    const outputEvents: Array<{ stream: string; text: string }> = [];

    await expect(
      runTerminalCommandStream(
        {
          command: "npm test",
          projectId: "project-a",
          sessionId: null,
          workdir: "C:/repo",
        },
        { onOutput: (event) => outputEvents.push(event) },
      ),
    ).resolves.toEqual(terminalResponseBody);

    expect(outputEvents).toEqual([
      { stream: "stdout", text: "building...\n" },
    ]);
  });

  it("drains multiple output frames from one buffered chunk", async () => {
    stubTerminalStream([
      [
        'event: output\ndata: {"stream":"stdout","text":"one\\n"}',
        "",
        'event: output\ndata: {"stream":"stderr","text":"two\\n"}',
        "",
        `event: complete\ndata: ${JSON.stringify(terminalResponseBody)}`,
        "",
        "",
      ].join("\n"),
    ]);
    const outputEvents: Array<{ stream: string; text: string }> = [];

    await expect(
      runTerminalCommandStream(
        {
          command: "npm test",
          projectId: "project-a",
          sessionId: null,
          workdir: "C:/repo",
        },
        { onOutput: (event) => outputEvents.push(event) },
      ),
    ).resolves.toEqual(terminalResponseBody);

    expect(outputEvents).toEqual([
      { stream: "stdout", text: "one\n" },
      { stream: "stderr", text: "two\n" },
    ]);
  });

  it("rejects malformed output and completion events", async () => {
    // Malformed-payload rejections are `422 Unprocessable Entity` via
    // `createMalformedTerminalStreamPayloadError`. Historically they
    // were `502`, but that collided with `createResponseError`'s
    // 502/503/504 → `kind: "backend-unavailable"` mapping on the same
    // route, so the same numeric status would produce two different
    // `ApiRequestError.kind` values depending on whether the remote
    // emitted a real HTTP 502 or a structurally-invalid SSE payload.
    // 422 is a status that `createResponseError` does NOT special-case,
    // which keeps the synthetic error shape aligned with the rest of
    // the codebase.
    stubTerminalStream([
      'event: output\ndata: {"stream":"warn","text":"heads up"}\n\n',
    ]);

    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "Terminal stream returned an invalid output event.",
      status: 422,
    });

    stubTerminalStream([
      'event: complete\ndata: {"error":"remote failed","status":429}\n\n',
    ]);

    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      message: "remote failed",
      status: 429,
    });
  });

  it("routes malformed SSE payloads away from the backend-unavailable UI path", async () => {
    // Regression guard for the 502/kind divergence the malformed-payload
    // validators used to have. A real HTTP 502 on the same route hits
    // `createResponseError`, produces `kind: "backend-unavailable"`, and
    // trips `isBackendUnavailableError` into the "local backend is
    // unavailable" overlay (with `restartRequired: true`). The SSE
    // payload validators must NOT share that shape for a malformed
    // `output` or `complete` frame — those are schema violations from
    // an otherwise-reachable remote, not an upstream gateway failure.
    //
    // This test catches the thrown error and runs
    // `isBackendUnavailableError` on the live object rather than relying
    // on `rejects.toMatchObject`: the existing sibling assertions pin
    // the observable shape (`kind`, `status`), but only an explicit
    // negative on the classifier proves the UI routing is unaffected.
    expect.assertions(8);

    stubTerminalStream([
      'event: output\ndata: {"stream":"warn","text":"heads up"}\n\n',
    ]);
    try {
      await runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      });
      throw new Error("Expected runTerminalCommandStream to reject");
    } catch (error) {
      expect(error).toBeInstanceOf(ApiRequestError);
      const apiError = error as ApiRequestError;
      expect(apiError.kind).toBe("request-failed");
      expect(apiError.status).toBe(422);
      expect(isBackendUnavailableError(apiError)).toBe(false);
    }

    stubTerminalStream([
      `event: complete\ndata: ${JSON.stringify({
        ...terminalResponseBody,
        command: 123,
      })}\n\n`,
    ]);
    try {
      await runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      });
      throw new Error("Expected runTerminalCommandStream to reject");
    } catch (error) {
      expect(error).toBeInstanceOf(ApiRequestError);
      const apiError = error as ApiRequestError;
      expect(apiError.kind).toBe("request-failed");
      expect(apiError.status).toBe(422);
      expect(isBackendUnavailableError(apiError)).toBe(false);
    }
  });

  it("rejects completion events with missing or wrong-typed fields", async () => {
    // Every branch in `parseTerminalCommandResponse`'s `typeof` guard needs
    // at least one explicit invalid-value case so that dropping or loosening
    // a single guard surfaces a concrete test failure. The original test
    // only covered `command` (wrong type) and missing `durationMs`, which
    // left eight other branches untested. Each entry here is labelled so
    // the failing row is easy to identify.
    //
    // Note on `NaN` / `Infinity`: `JSON.stringify(NaN)` and
    // `JSON.stringify(Infinity)` both serialize to `null`, so a
    // `{ durationMs: NaN }` object round-trips through the SSE frame as
    // `{"durationMs":null}` and is caught by the `typeof !== "number"`
    // branch, not the `!Number.isFinite` branch. The real non-finite
    // numeric case (where `JSON.parse` returns an actual `Infinity`) is
    // covered by
    // `rejects completion events whose numeric fields parse as Infinity`
    // below, which skips `JSON.stringify` and feeds a literal `1e500`
    // into the frame.
    const missingDuration: Partial<typeof terminalResponseBody> = {
      ...terminalResponseBody,
    };
    delete missingDuration.durationMs;
    const invalidCompletions: Array<[string, unknown]> = [
      ["command wrong type", { ...terminalResponseBody, command: 123 }],
      ["durationMs missing", missingDuration],
      ["durationMs null", { ...terminalResponseBody, durationMs: null }],
      ["exitCode wrong type (string)", { ...terminalResponseBody, exitCode: "0" }],
      ["exitCode wrong type (boolean)", { ...terminalResponseBody, exitCode: true }],
      [
        "outputTruncated wrong type",
        { ...terminalResponseBody, outputTruncated: "false" },
      ],
      ["shell wrong type", { ...terminalResponseBody, shell: 42 }],
      ["stderr wrong type", { ...terminalResponseBody, stderr: null }],
      ["stdout wrong type", { ...terminalResponseBody, stdout: 0 }],
      ["success wrong type", { ...terminalResponseBody, success: 1 }],
      ["timedOut wrong type", { ...terminalResponseBody, timedOut: "no" }],
      ["workdir wrong type", { ...terminalResponseBody, workdir: null }],
    ];

    for (const [label, invalidCompletion] of invalidCompletions) {
      stubTerminalStream([
        `event: complete\ndata: ${JSON.stringify(invalidCompletion)}\n\n`,
      ]);

      await expect(
        runTerminalCommandStream({
          command: "npm test",
          projectId: "project-a",
          sessionId: null,
          workdir: "C:/repo",
        }),
        `invalid completion case "${label}" should reject`,
      ).rejects.toMatchObject({
        kind: "request-failed",
        message: "Terminal stream returned an invalid completion event.",
        status: 422,
      });
    }
  });

  it("rejects completion events whose numeric fields parse as Infinity", async () => {
    // `1e500` is a valid JSON number literal that `JSON.parse` returns as
    // `Infinity` (it overflows `Number.MAX_VALUE`). This is the only way
    // to drive a real non-finite number through `parseTerminalCommandResponse`
    // because `JSON.stringify(Infinity)` produces `null` (and `NaN`
    // likewise). The `Number.isFinite` guards on `durationMs` and
    // `exitCode` defend against exactly this path — without them, an
    // `Infinity` exit code would flow through as a "valid" result and
    // corrupt downstream `exitCode === 0` checks.
    const baseJson = JSON.stringify(terminalResponseBody);

    const infiniteDuration = baseJson.replace(
      /"durationMs":42/,
      '"durationMs":1e500',
    );
    expect(infiniteDuration).not.toBe(baseJson);
    stubTerminalStream([`event: complete\ndata: ${infiniteDuration}\n\n`]);
    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "Terminal stream returned an invalid completion event.",
      status: 422,
    });

    const infiniteExitCode = baseJson.replace(
      /"exitCode":0/,
      '"exitCode":1e500',
    );
    expect(infiniteExitCode).not.toBe(baseJson);
    stubTerminalStream([`event: complete\ndata: ${infiniteExitCode}\n\n`]);
    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "Terminal stream returned an invalid completion event.",
      status: 422,
    });
  });

  it("accepts completion events whose exitCode is null", async () => {
    // The validator treats `null` / `undefined` as "process never exited
    // normally" and must not reject them alongside the new
    // `Number.isFinite` tightening for numeric exit codes.
    const response = { ...terminalResponseBody, exitCode: null };
    stubTerminalStream([
      `event: complete\ndata: ${JSON.stringify(response)}\n\n`,
    ]);

    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).resolves.toEqual(response);
  });

  it("falls back to a status-only message when an error event is valid JSON but omits the error field", async () => {
    // `createTerminalStreamEventError` parses the SSE error frame's data
    // payload and uses `parsed.error ?? 'Request failed with status ${status}.'`
    // when the JSON is valid but does not carry an `error` field. The
    // malformed-JSON branch is already covered by the
    // `surfaces raw text from malformed stream error events` test; this
    // case pins the parsed-but-missing-error fallback so a regression in
    // the nullish-coalesce would fail a specific test.
    stubTerminalStream(['event: error\ndata: {"status":502}\n\n']);

    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      message: "Request failed with status 502.",
      status: 502,
    });
  });

  it("rejects streams that end without a completion event", async () => {
    stubTerminalStream([
      'event: output\ndata: {"stream":"stdout","text":"partial\\n"}\n\n',
    ]);

    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      message: "Terminal stream ended before the command completed.",
      status: 500,
    });
  });

  it("surfaces raw text from malformed stream error events", async () => {
    stubTerminalStream(["event: error\ndata: not-json\n\n"]);

    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      message: "not-json",
      status: 500,
    });
  });

  it("surfaces embedded stream errors without treating 502 as local backend outage", async () => {
    stubTerminalStream([
      'event: error\ndata: {"error":"remote proxy failed","status":502}\n\n',
    ]);

    await expect(
      runTerminalCommandStream({
        command: "npm test",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "remote proxy failed",
      restartRequired: false,
      status: 502,
    });
  });

  it("accepts a completion frame carrying the backend's maximum-sized output", async () => {
    // A legitimate completion frame can carry up to 512 KiB of stdout plus
    // the same of stderr (the backend `TERMINAL_OUTPUT_MAX_BYTES`). When the
    // output is newline-heavy each `\n` byte JSON-encodes to the two-char
    // escape `\\n`, so the serialized completion frame weighs roughly 2 MiB
    // + envelope. Before the buffer cap was raised, this valid frame tripped
    // `assertTerminalSseBufferSize` with "Terminal stream frame exceeded the
    // allowed size." The new 8 Mi-char cap must let it through.
    const fill = "\n".repeat(512 * 1024);
    const largeResponse = {
      ...terminalResponseBody,
      stdout: fill,
      stderr: fill,
    };
    const completionFrame = `event: complete\ndata: ${JSON.stringify(largeResponse)}\n\n`;
    // Regression guard: the serialized frame must clearly exceed the
    // pre-fix 2 Mi-char cap so this test would fail if the cap regressed.
    expect(completionFrame.length).toBeGreaterThan(2 * 1024 * 1024);
    stubTerminalStream([completionFrame]);

    await expect(
      runTerminalCommandStream({
        command: "cat big.log",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).resolves.toEqual(largeResponse);
  });

  it("rejects an unterminated frame larger than the SSE buffer cap", async () => {
    // The cap still bounds memory: if the backend (or a remote proxy) stalls
    // midway through a frame without emitting a delimiter, the reader must
    // surface "Terminal stream frame exceeded the allowed size." rather than
    // buffer indefinitely. Use a chunk that is clearly larger than the new
    // cap so the test does not depend on the exact cap value.
    //
    // Status is 413 (Payload Too Large), intentionally distinct from the
    // 502/503/504 that `createResponseError` maps to `kind:
    // "backend-unavailable"`. A cap rejection is a payload violation, not
    // an upstream gateway failure. Picking 413 keeps both the per-frame
    // and trailing-buffer throws aligned with the rest of the codebase's
    // error-kind conventions and avoids the "same status, two different
    // `ApiRequestError.kind` values" divergence that an HTTP 502 had.
    const oversizedChunk = "x".repeat(9 * 1024 * 1024);
    stubTerminalStream([oversizedChunk]);

    await expect(
      runTerminalCommandStream({
        command: "cat huge.bin",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "Terminal stream frame exceeded the allowed size.",
      status: 413,
    });
  });

  it("accepts coalesced valid frames whose combined length exceeds the cap", async () => {
    // Regression for the pre-fix whole-buffer cap check. Fetch chunks are
    // not guaranteed to align with SSE frame boundaries: the browser can
    // coalesce an `output` frame and a `complete` frame into a single
    // `reader.read()` result. When each frame is individually under
    // `TERMINAL_SSE_BUFFER_MAX_CHARS` but their concatenated buffer text
    // exceeds it, the old code threw "Terminal stream frame exceeded the
    // allowed size." before `processTerminalSseBuffer` had a chance to
    // drain the parsed frames. The fix drains each complete frame first,
    // enforces the cap per frame, and only checks the trailing incomplete
    // buffer after the drain. This test drives that exact scenario: build
    // a ~5 Mi-char `output` frame plus a completion frame that together
    // clearly exceed the 8 Mi-char cap, coalesce them into a single
    // stubbed chunk, and assert the stream resolves with the completion
    // response and the output event was observed.
    const outputFill = "x".repeat(5 * 1024 * 1024);
    const completionStdout = "y".repeat(3 * 1024 * 1024);
    const completionResponse = {
      ...terminalResponseBody,
      stdout: completionStdout,
    };
    const outputFrame = `event: output\ndata: ${JSON.stringify({
      stream: "stdout",
      text: outputFill,
    })}\n\n`;
    const completionFrame = `event: complete\ndata: ${JSON.stringify(completionResponse)}\n\n`;
    const coalesced = outputFrame + completionFrame;
    // Regression guards: each individual frame must fit, and the coalesced
    // buffer must clearly exceed the cap so the pre-fix whole-buffer check
    // would have rejected it.
    expect(outputFrame.length).toBeLessThan(16 * 512 * 1024);
    expect(completionFrame.length).toBeLessThan(16 * 512 * 1024);
    expect(coalesced.length).toBeGreaterThan(16 * 512 * 1024);

    const outputEvents: Array<{ stream: string; text: string }> = [];
    stubTerminalStream([coalesced]);

    await expect(
      runTerminalCommandStream(
        {
          command: "cat big.log",
          projectId: "project-a",
          sessionId: null,
          workdir: "C:/repo",
        },
        { onOutput: (event) => outputEvents.push(event) },
      ),
    ).resolves.toEqual(completionResponse);
    expect(outputEvents).toEqual([
      { stream: "stdout", text: outputFill },
    ]);
  });

  it("rejects a single complete frame whose length exceeds the SSE buffer cap", async () => {
    // Regression guard for the per-frame check inside
    // `processTerminalSseBuffer`. Without it, a 9 Mi-char `output` frame
    // followed by `\n\n` would slip past the trailing-buffer check after
    // drain (the drain removes the oversized frame, leaving a nearly
    // empty buffer) and hand the oversized text to `parseTerminalOutputEvent`
    // unchecked. The per-frame guard must reject the frame before the
    // drain consumes it, so the stream surfaces
    // "Terminal stream frame exceeded the allowed size." exactly like the
    // unterminated-frame case above.
    const oversizedFrame = `event: output\ndata: ${JSON.stringify({
      stream: "stdout",
      text: "x".repeat(9 * 1024 * 1024),
    })}\n\n`;
    expect(oversizedFrame.length).toBeGreaterThan(16 * 512 * 1024);
    stubTerminalStream([oversizedFrame]);

    await expect(
      runTerminalCommandStream({
        command: "cat huge.bin",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "Terminal stream frame exceeded the allowed size.",
      status: 413,
    });
  });

  it("pins the SSE buffer cap to the backend-coupled production value", () => {
    // Mirror of the Rust-side
    // `forward_remote_terminal_stream_reader_uses_production_cap_at_least_as_large_as_max_frame`
    // sanity check. The backend computes its remote SSE pending-buffer cap
    // as `TERMINAL_OUTPUT_MAX_BYTES * 16` in `src/api.rs` (where
    // `TERMINAL_OUTPUT_MAX_BYTES = 512 KiB`). If the frontend literal ever
    // drifts below the backend-coupled value, a legitimate completion
    // frame that the backend forwards unchanged would be rejected
    // client-side with "Terminal stream frame exceeded the allowed size."
    // This test pins the coupling so either side breaks loudly under
    // edit.
    const backendTerminalOutputMaxBytes = 512 * 1024;
    const backendPendingCap = backendTerminalOutputMaxBytes * 16;
    expect(TERMINAL_SSE_BUFFER_MAX_CHARS).toBeGreaterThanOrEqual(
      backendPendingCap,
    );
  });

  it("preserves the original cap error when reader.cancel() rejects during the finally block", async () => {
    // Regression for the defensive `reader.cancel().catch(() => {})` in
    // `readTerminalCommandEventStream`'s finally block. Without the
    // swallowing `.catch`, a cancel-side rejection inside `finally` would
    // replace whatever error the try block already threw — e.g. the
    // oversized-chunk cap error — with a generic "transport gone" style
    // error. Every other terminal stream test relies on a default
    // `ReadableStream` whose `cancel()` resolves silently, so a regression
    // that dropped the `.catch` would slip through unnoticed. This test
    // wires a custom `ReadableStream` whose underlying source's `cancel`
    // method throws, drives the reader into an early-error exit via an
    // oversized unterminated chunk, and asserts the rejection still
    // surfaces the ORIGINAL "Terminal stream frame exceeded the allowed
    // size." error rather than the cancel-side rejection.
    const encoder = new TextEncoder();
    const oversizedChunk = encoder.encode("x".repeat(9 * 1024 * 1024));
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(oversizedChunk);
        // Intentionally leave the stream open so the reader hits its
        // oversized-frame rejection path before it would otherwise see
        // `done: true`.
      },
      cancel() {
        throw new Error("transport gone");
      },
    });
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(stream, {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      runTerminalCommandStream({
        command: "cat huge.bin",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "Terminal stream frame exceeded the allowed size.",
      status: 413,
    });
  });

  it("delivers valid output events before throwing on a later oversized frame in the same chunk", async () => {
    // Regression for the drain-then-throw mid-chunk path. The existing
    // cap tests cover (a) two coalesced valid frames whose sum exceeds
    // the cap and (b) a single oversized frame on its own. Neither
    // exercises the intermediate case: a chunk whose earlier frames are
    // individually valid, get drained and forwarded, and a later frame
    // in the SAME chunk then trips the per-frame cap. A future edit that
    // moved the per-frame guard out of `processTerminalSseBuffer` would
    // break this path without failing any other test. This test verifies
    // that the first valid `output` event reaches `onOutput` before the
    // rejection fires on the oversized frame, and that the `complete`
    // frame after the oversized one is NOT parsed.
    const validOutputFrame =
      'event: output\ndata: {"stream":"stdout","text":"first"}\n\n';
    const oversizedOutputFrame = `event: output\ndata: ${JSON.stringify({
      stream: "stdout",
      text: "x".repeat(9 * 1024 * 1024),
    })}\n\n`;
    const completionFrame = `event: complete\ndata: ${JSON.stringify(
      terminalResponseBody,
    )}\n\n`;
    const coalesced =
      validOutputFrame + oversizedOutputFrame + completionFrame;
    stubTerminalStream([coalesced]);

    const outputEvents: Array<{ stream: string; text: string }> = [];

    await expect(
      runTerminalCommandStream(
        {
          command: "cat big.log",
          projectId: "project-a",
          sessionId: null,
          workdir: "C:/repo",
        },
        { onOutput: (event) => outputEvents.push(event) },
      ),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "Terminal stream frame exceeded the allowed size.",
      status: 413,
    });
    expect(outputEvents).toEqual([{ stream: "stdout", text: "first" }]);
  });

  it("rejects incrementally as the buffer grows across chunks without an open-ended wait", async () => {
    // Regression for the trailing-buffer check inside the incremental
    // read loop (not the post-drain flush at stream end). Both new
    // oversized-frame tests use single-chunk delivery, so a regression
    // that moved `assertTerminalSseBufferSize` out of the in-loop path
    // and into the post-loop flush block would still pass them: the
    // flush-time guard would catch the same case. This test drives
    // several 3 MiB undelimited chunks (each individually under the cap,
    // cumulatively over) into a `ReadableStream` that is NEVER closed.
    // The in-loop check must fire after the third chunk's
    // `processTerminalSseBuffer` returns the growing trailing buffer; a
    // post-loop-only check would stall forever waiting for more data
    // and the Vitest per-test timeout would fail the test loudly.
    const encoder = new TextEncoder();
    const chunk = encoder.encode("x".repeat(3 * 1024 * 1024));
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(chunk);
        controller.enqueue(chunk);
        controller.enqueue(chunk);
        // Deliberately do NOT close: the reader must detect the
        // oversized trailing buffer and throw before it would otherwise
        // observe `done: true`.
      },
    });
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(stream, {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      runTerminalCommandStream({
        command: "cat huge.bin",
        projectId: "project-a",
        sessionId: null,
        workdir: "C:/repo",
      }),
    ).rejects.toMatchObject({
      kind: "request-failed",
      message: "Terminal stream frame exceeded the allowed size.",
      status: 413,
    });
  }, 2000);
});

describe("deleteWorkspaceLayout", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("uses DELETE with an encoded workspace id and parses the remaining summaries", async () => {
    const responseBody = {
      workspaces: [
        {
          id: "workspace-two",
          revision: 9,
          updatedAt: "2026-04-06 10:15:00",
          controlPanelSide: "right",
        },
      ],
    };
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(responseBody), {
        status: 200,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const result = await deleteWorkspaceLayout("workspace/one two");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/workspaces/workspace%2Fone%20two",
      expect.objectContaining({
        method: "DELETE",
      }),
    );
    expect(result).toEqual(responseBody);
  });
});

describe("fetchState", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("classifies proxy 502 failures as structured backend-unavailable errors", async () => {
    expect.assertions(3);
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: "proxy failed while reading C:\\internal\\server.ts",
          }),
          {
            status: 502,
            headers: {
              "Content-Type": "application/json",
            },
          },
        ),
      ),
    );

    try {
      await fetchState();
      throw new Error("Expected fetchState to reject");
    } catch (error) {
      expect(isBackendUnavailableError(error)).toBe(true);
      expect(error).toBeInstanceOf(Error);
      expect((error as Error).message).toBe(
        "The TermAl backend is unavailable.",
      );
    }
  });

  it("classifies proxy 503 and 504 failures as structured backend-unavailable errors", async () => {
    expect.assertions(6);

    for (const status of [503, 504]) {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(
          new Response(JSON.stringify({ error: `proxy failed (${status})` }), {
            status,
            headers: {
              "Content-Type": "application/json",
            },
          }),
        ),
      );

      try {
        await fetchState();
        throw new Error(`Expected fetchState to reject for HTTP ${status}`);
      } catch (error) {
        expect(isBackendUnavailableError(error)).toBe(true);
        expect((error as ApiRequestError).status).toBe(status);
        expect((error as Error).message).toBe(
          "The TermAl backend is unavailable.",
        );
      }
    }
  });

  it("preserves the original fetch rejection on ApiRequestError.cause", async () => {
    expect.assertions(4);
    const rootCause = new TypeError("network unreachable");
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(rootCause));

    try {
      await fetchState();
      throw new Error("Expected fetchState to reject");
    } catch (error) {
      expect(isBackendUnavailableError(error)).toBe(true);
      expect(error).toBeInstanceOf(ApiRequestError);
      expect((error as ApiRequestError).cause).toBe(rootCause);
      expect((error as ApiRequestError).status).toBeNull();
    }
  });

  it("sets restartRequired on HTML fallback errors from an incompatible backend", async () => {
    expect.assertions(4);
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          "<!DOCTYPE html><html><body>Old backend</body></html>",
          {
            status: 200,
            headers: {
              "Content-Type": "text/html",
            },
          },
        ),
      ),
    );

    try {
      await fetchState();
      throw new Error("Expected fetchState to reject");
    } catch (error) {
      expect(isBackendUnavailableError(error)).toBe(true);
      expect(error).toBeInstanceOf(ApiRequestError);
      expect((error as ApiRequestError).restartRequired).toBe(true);
      expect((error as Error).message).toContain("Restart TermAl");
    }
  });
});

describe("fetchWorkspaceLayout", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("returns null for JSON 404 responses", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ error: "layout missing" }), {
        status: 404,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchWorkspaceLayout("workspace/one two")).resolves.toBeNull();
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/workspaces/workspace%2Fone%20two",
      expect.objectContaining({
        headers: expect.objectContaining({
          "Content-Type": "application/json",
        }),
      }),
    );
  });

  it("treats HTML 404 fallbacks as restart-required backend errors", async () => {
    expect.assertions(5);
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response("<!DOCTYPE html><html><body>Not found</body></html>", {
          status: 404,
          headers: {
            "Content-Type": "text/html",
          },
        }),
      ),
    );

    try {
      await fetchWorkspaceLayout("workspace/one two");
      throw new Error("Expected fetchWorkspaceLayout to reject");
    } catch (error) {
      expect(isBackendUnavailableError(error)).toBe(true);
      expect(error).toBeInstanceOf(ApiRequestError);
      expect((error as ApiRequestError).restartRequired).toBe(true);
      expect((error as Error).message).toContain(
        "/api/workspaces/workspace%2Fone%20two (HTTP 404)",
      );
      expect((error as Error).message).not.toContain("/api/workspaces/workspace/one two");
    }
  });
});
