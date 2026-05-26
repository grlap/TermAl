// Owns terminal command API wrappers and terminal SSE stream parsing.
// Does not own generic request/error helpers or unrelated typed route wrappers.
// Split from: ui/src/api.ts.
import {
  ApiRequestError,
  createBackendUnavailableError,
  createResponseError,
  formatUnavailableApiMessage,
  looksLikeHtmlResponse,
  performRequest,
  request,
} from "./api-request";
import { sanitizeUserFacingErrorMessage } from "./error-messages";

// Upper bound on how much SSE text the terminal stream reader will buffer
// before surfacing an error. A completion frame carries the full terminal
// response, which can include up to 512 KiB of stdout plus the same of
// stderr. JSON encoding can expand each byte up to 6x for ASCII control
// characters (`\u00XX`), and common newline-heavy output already doubles the
// raw size, so cap at 16x the backend output budget (8 Mi chars) to let
// legitimate completion frames through while still bounding memory if a
// remote stalls without emitting a frame delimiter.
//
// Exported so `ui/src/api.test.ts` can pin the frontend <-> backend cap
// coupling without duplicating the derivation formula on both sides. The
// backend computes its equivalent pending cap as
// `TERMINAL_OUTPUT_MAX_BYTES * 16` in `src/api.rs`; if either side drifts
// the other's regression test fails.
export const TERMINAL_SSE_BUFFER_MAX_CHARS = 16 * 512 * 1024;

export type TerminalCommandResponse = {
  command: string;
  durationMs: number;
  exitCode?: number | null;
  outputTruncated: boolean;
  shell: string;
  stderr: string;
  stdout: string;
  success: boolean;
  timedOut: boolean;
  workdir: string;
};

export type TerminalOutputStream = "stdout" | "stderr";

export type TerminalCommandOutputEvent = {
  stream: TerminalOutputStream;
  text: string;
};

type TerminalCommandStreamErrorEvent = {
  error?: string;
  status?: number;
};

export function runTerminalCommand(payload: {
  command: string;
  sessionId?: string | null;
  projectId?: string | null;
  workdir: string;
}) {
  return request<TerminalCommandResponse>("/api/terminal/run", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function runTerminalCommandStream(
  payload: {
    command: string;
    sessionId?: string | null;
    projectId?: string | null;
    workdir: string;
  },
  options: {
    onOutput?: (event: TerminalCommandOutputEvent) => void;
    signal?: AbortSignal;
  } = {},
) {
  const endpoint = "/api/terminal/run/stream";
  const response = await performRequest(endpoint, {
    method: "POST",
    body: JSON.stringify(payload),
    signal: options.signal,
  });

  const contentType = response.headers.get("content-type") ?? "";
  if (looksLikeHtmlResponse("", contentType)) {
    await response.text().catch(() => "");
    throw createBackendUnavailableError(
      formatUnavailableApiMessage(endpoint, response.status),
      response.status,
      { restartRequired: true },
    );
  }

  if (!response.ok) {
    const raw = await response.text();
    throw createResponseError(raw, response.status);
  }

  if (!response.body) {
    throw createBackendUnavailableError(
      "The TermAl backend did not return a terminal stream.",
      response.status,
    );
  }

  return readTerminalCommandEventStream(response.body, options.onOutput);
}

async function readTerminalCommandEventStream(
  body: ReadableStream<Uint8Array>,
  onOutput: ((event: TerminalCommandOutputEvent) => void) | undefined,
) {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let completed = false;

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      buffer = normalizeSseBuffer(
        buffer + decoder.decode(value, { stream: true }),
      );
      // Drain every complete frame first so that coalesced valid frames
      // whose accumulated text exceeds `TERMINAL_SSE_BUFFER_MAX_CHARS` are
      // not rejected before parsing. `processTerminalSseBuffer` enforces
      // the per-frame cap on each drained frame, and the post-drain
      // `assertTerminalSseBufferSize` check bounds the trailing incomplete
      // buffer so a single in-flight frame cannot grow unboundedly across
      // chunks. Without the drain-first order, a fetch chunk that carries
      // a valid `output` plus a valid `complete` frame (each under the
      // cap) would trip the old whole-buffer check whenever their sum
      // crossed the cap.
      const result = processTerminalSseBuffer(buffer, onOutput);
      buffer = result.buffer;
      assertTerminalSseBufferSize(buffer.length);
      if (result.response) {
        completed = true;
        return result.response;
      }
    }
    buffer = normalizeSseBuffer(buffer + decoder.decode());
    const result = processTerminalSseBuffer(buffer, onOutput, true);
    buffer = result.buffer;
    assertTerminalSseBufferSize(buffer.length);
    if (result.response) {
      completed = true;
      return result.response;
    }

    throw new ApiRequestError(
      "request-failed",
      "Terminal stream ended before the command completed.",
      { status: 500 },
    );
  } finally {
    if (!completed) {
      await reader.cancel().catch(() => {});
    }
    reader.releaseLock();
  }
}

function processTerminalSseBuffer(
  buffer: string,
  onOutput: ((event: TerminalCommandOutputEvent) => void) | undefined,
  flush = false,
) {
  let remaining = buffer;
  let response: TerminalCommandResponse | null = null;

  while (true) {
    const frameEnd = remaining.indexOf("\n\n");
    if (frameEnd < 0) {
      if (!flush || remaining.trim() === "") {
        break;
      }
    }
    // Each individual SSE frame must stay within the buffer cap. The
    // `readTerminalCommandEventStream` loop drains complete frames before
    // re-checking the trailing buffer, so without this per-frame guard a
    // malicious or broken remote could smuggle a single frame of arbitrary
    // size inside one coalesced fetch chunk: the drain would consume it,
    // the post-drain check would see an empty trailing buffer, and the
    // parser would accept the oversized frame outright. Routed through
    // the same `assertTerminalSseBufferSize` helper as the trailing-
    // buffer check so both cap violations share one error shape (413 +
    // `request-failed`) and never drift out of sync.
    const frameLength = frameEnd >= 0 ? frameEnd : remaining.length;
    assertTerminalSseBufferSize(frameLength);
    const frame = frameEnd >= 0 ? remaining.slice(0, frameEnd) : remaining;
    remaining = frameEnd >= 0 ? remaining.slice(frameEnd + 2) : "";
    if (!frame.trim()) {
      if (frameEnd < 0) {
        break;
      }
      continue;
    }

    const parsed = parseSseFrame(frame);
    if (parsed.event === "output") {
      const output = parseTerminalOutputEvent(parsed.data);
      onOutput?.(output);
    } else if (parsed.event === "complete") {
      response = parseTerminalCommandResponse(parsed.data);
      break;
    } else if (parsed.event === "error") {
      throw createTerminalStreamEventError(parsed.data);
    }

    if (frameEnd < 0) {
      break;
    }
  }

  return { buffer: remaining, response };
}

function normalizeSseBuffer(value: string) {
  return value.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

// Shared guard for the two SSE buffer-cap violation sites:
//
// 1. `processTerminalSseBuffer` passes the length of a single drained
//    frame, so no individual frame can slip past the cap even when it
//    arrives inside a coalesced chunk that would also fit another valid
//    frame (which the drain-first loop would otherwise let through).
// 2. `readTerminalCommandEventStream` passes the length of the trailing
//    incomplete buffer after draining every complete frame, so an
//    in-flight frame cannot grow unboundedly across chunks.
//
// The thrown status is `413 Payload Too Large`, intentionally distinct
// from the 502/503/504 statuses that `createResponseError` maps to
// `kind: "backend-unavailable"`. A cap rejection is a semantic payload
// violation, not an upstream gateway failure, and using a status that
// `createResponseError` does NOT special-case keeps the synthetic
// cap-rejection error shape aligned with the rest of the codebase
// regardless of how the same route handles a real HTTP 502 response.
function assertTerminalSseBufferSize(length: number) {
  if (length <= TERMINAL_SSE_BUFFER_MAX_CHARS) {
    return;
  }
  throw new ApiRequestError(
    "request-failed",
    "Terminal stream frame exceeded the allowed size.",
    { status: 413 },
  );
}

function parseSseFrame(frame: string) {
  let event = "message";
  const data: string[] = [];

  for (const line of frame.split("\n")) {
    if (line.startsWith(":")) {
      continue;
    }
    const separator = line.indexOf(":");
    const field = separator >= 0 ? line.slice(0, separator) : line;
    let value = separator >= 0 ? line.slice(separator + 1) : "";
    if (value.startsWith(" ")) {
      value = value.slice(1);
    }
    if (field === "event") {
      event = value;
    } else if (field === "data") {
      data.push(value);
    }
  }

  return { data: data.join("\n"), event };
}

// Shared constructor for malformed SSE payload validation errors thrown by
// `parseTerminalOutputEvent` and `parseTerminalCommandResponse`.
//
// Status is `422 Unprocessable Entity`, intentionally distinct from the
// 502/503/504 statuses that `createResponseError` maps to
// `kind: "backend-unavailable"`. A malformed SSE payload from an
// otherwise-reachable backend is a schema violation on a single stream,
// not an upstream gateway failure. Surfacing it as `status: 502` on the
// `"request-failed"` kind would leave the same numeric status attached
// to two different `ApiRequestError.kind` values on the same route (the
// pre-stream `if (!response.ok)` branch in `runTerminalCommandStream`
// uses `createResponseError` for real HTTP 502 responses and yields
// `kind: "backend-unavailable" / restartRequired: true`). 422 avoids
// that divergence because `createResponseError` does not special-case
// it, and the semantic "valid HTTP request, malformed response body"
// is exactly what 422 is meant for.
//
// This is the same discipline `assertTerminalSseBufferSize` uses for its
// 413 choice: pick a status that `createResponseError` does NOT
// special-case so the synthetic error shape stays consistent with the
// rest of the codebase.
function createMalformedTerminalStreamPayloadError(message: string) {
  return new ApiRequestError("request-failed", message, { status: 422 });
}

function parseTerminalOutputEvent(raw: string): TerminalCommandOutputEvent {
  const parsed = JSON.parse(raw) as unknown;
  if (
    !isRecord(parsed) ||
    (parsed.stream !== "stdout" && parsed.stream !== "stderr") ||
    typeof parsed.text !== "string"
  ) {
    throw createMalformedTerminalStreamPayloadError(
      "Terminal stream returned an invalid output event.",
    );
  }

  return {
    stream: parsed.stream,
    text: parsed.text,
  };
}

function parseTerminalCommandResponse(raw: string): TerminalCommandResponse {
  const parsed = JSON.parse(raw) as unknown;
  if (isRecord(parsed) && typeof parsed.error === "string") {
    throw createTerminalStreamEventError(raw);
  }
  if (
    !isRecord(parsed) ||
    typeof parsed.command !== "string" ||
    typeof parsed.durationMs !== "number" ||
    !Number.isFinite(parsed.durationMs) ||
    // `exitCode` is optional (`null`/`undefined` mean "process was killed
    // or never exited normally"), but when it IS a number it must be
    // finite. Without the `Number.isFinite` branch a remote or malformed
    // JSON payload carrying `NaN` / `Infinity` (the latter via
    // non-standard JSON extensions or a misbehaving proxy) would slip
    // through the validator and corrupt downstream exit-code checks.
    !(
      (typeof parsed.exitCode === "number" &&
        Number.isFinite(parsed.exitCode)) ||
      parsed.exitCode === null ||
      parsed.exitCode === undefined
    ) ||
    typeof parsed.outputTruncated !== "boolean" ||
    typeof parsed.shell !== "string" ||
    typeof parsed.stderr !== "string" ||
    typeof parsed.stdout !== "string" ||
    typeof parsed.success !== "boolean" ||
    typeof parsed.timedOut !== "boolean" ||
    typeof parsed.workdir !== "string"
  ) {
    throw createMalformedTerminalStreamPayloadError(
      "Terminal stream returned an invalid completion event.",
    );
  }

  return {
    command: parsed.command,
    durationMs: parsed.durationMs,
    exitCode: parsed.exitCode,
    outputTruncated: parsed.outputTruncated,
    shell: parsed.shell,
    stderr: parsed.stderr,
    stdout: parsed.stdout,
    success: parsed.success,
    timedOut: parsed.timedOut,
    workdir: parsed.workdir,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function createTerminalStreamEventError(raw: string) {
  try {
    const parsed = JSON.parse(raw) as TerminalCommandStreamErrorEvent;
    const status = parsed.status ?? 500;
    return new ApiRequestError(
      "request-failed",
      sanitizeUserFacingErrorMessage(
        parsed.error ?? `Request failed with status ${status}.`,
      ),
      { status },
    );
  } catch {
    return new ApiRequestError(
      "request-failed",
      sanitizeUserFacingErrorMessage(raw || "Request failed with status 500."),
      { status: 500 },
    );
  }
}
