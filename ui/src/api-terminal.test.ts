import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiRequestError } from "./api-request";
import {
  runTerminalCommand,
  runTerminalCommandStream,
  type TerminalCommandOutputEvent,
  type TerminalCommandResponse,
} from "./api-terminal";

afterEach(() => {
  vi.unstubAllGlobals();
});

function streamFromText(text: string) {
  const encoder = new TextEncoder();
  return new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(encoder.encode(text));
      controller.close();
    },
  });
}

function terminalResponse(overrides: Partial<TerminalCommandResponse> = {}): TerminalCommandResponse {
  return {
    command: "printf hello",
    durationMs: 12,
    exitCode: 0,
    outputTruncated: false,
    shell: "/bin/zsh",
    stderr: "",
    stdout: "hello",
    success: true,
    timedOut: false,
    workdir: "/tmp",
    ...overrides,
  };
}

describe("api-terminal", () => {
  it("runs terminal commands through the typed route wrapper", async () => {
    const response = terminalResponse();
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(response), {
        headers: { "content-type": "application/json" },
        status: 200,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      runTerminalCommand({ command: "printf hello", workdir: "/tmp" }),
    ).resolves.toEqual(response);
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/terminal/run",
      expect.objectContaining({
        body: JSON.stringify({ command: "printf hello", workdir: "/tmp" }),
        method: "POST",
      }),
    );
  });

  it("parses terminal stream output and completion frames", async () => {
    const response = terminalResponse();
    const outputEvents: TerminalCommandOutputEvent[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          streamFromText(
            [
              `event: output\ndata: ${JSON.stringify({ stream: "stdout", text: "he" })}`,
              `event: output\ndata: ${JSON.stringify({ stream: "stdout", text: "llo" })}`,
              `event: complete\ndata: ${JSON.stringify(response)}`,
            ].join("\n\n") + "\n\n",
          ),
          {
            headers: { "content-type": "text/event-stream" },
            status: 200,
          },
        ),
      ),
    );

    await expect(
      runTerminalCommandStream(
        { command: "printf hello", workdir: "/tmp" },
        { onOutput: (event) => outputEvents.push(event) },
      ),
    ).resolves.toEqual(response);
    expect(outputEvents).toEqual([
      { stream: "stdout", text: "he" },
      { stream: "stdout", text: "llo" },
    ]);
  });

  it("rejects malformed terminal completion frames as request failures", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          streamFromText(
            `event: complete\ndata: ${JSON.stringify({ command: "x", durationMs: "bad" })}\n\n`,
          ),
          {
            headers: { "content-type": "text/event-stream" },
            status: 200,
          },
        ),
      ),
    );

    let caught: unknown = null;
    try {
      await runTerminalCommandStream({ command: "printf hello", workdir: "/tmp" });
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(ApiRequestError);
    expect(caught).toMatchObject({
      kind: "request-failed",
      status: 422,
    });
  });
});
