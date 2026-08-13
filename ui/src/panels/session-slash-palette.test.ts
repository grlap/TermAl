import { describe, expect, it } from "vitest";

import {
  CLAUDE_EFFORT_SLASH_OPTIONS,
  FALLBACK_CLAUDE_EFFORTS,
  codexFastSlashState,
  codexMcpSlashState,
  opencodeEffortSlashState,
  sessionModeSlashState,
  sessionModelSlashState,
  slashCommandsForSession,
  type SlashPaletteSession,
} from "./session-slash-palette";

describe("Claude effort slash choices", () => {
  it("exposes xhigh in the slash palette fallback choices", () => {
    expect(CLAUDE_EFFORT_SLASH_OPTIONS.map((option) => option.value)).toEqual([
      "default",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
    expect(FALLBACK_CLAUDE_EFFORTS).toEqual(["low", "medium", "high", "xhigh"]);
  });
});

describe("Codex Fast slash choices", () => {
  const session = {
    id: "session-codex",
    agent: "Codex",
    agentCommandsRevision: 0,
    model: "gpt-5.5",
    modelOptions: [
      {
        value: "gpt-5.5",
        label: "GPT-5.5",
        serviceTiers: [
          { id: "priority", label: "Fast", description: "1.5x speed, increased usage" },
        ],
      },
    ],
    codexFastMode: true,
    workdir: "/tmp",
  } as SlashPaletteSession;

  it("keeps /fast discoverable even before support is known", () => {
    expect(slashCommandsForSession(session).map((command) => command.id)).toContain("fast");
    expect(
      slashCommandsForSession({
        ...session,
        codexFastMode: false,
        modelOptions: [{ value: "gpt-5.5", label: "GPT-5.5" }],
      }).map((command) => command.id),
    ).toContain("fast");
    expect(
      slashCommandsForSession({
        ...session,
        modelOptions: undefined,
      }).map((command) => command.id),
    ).toContain("fast");
  });

  it("marks the current Fast choice", () => {
    expect(codexFastSlashState(session, "").items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ value: "off", isCurrent: false }),
        expect.objectContaining({ value: "on", isCurrent: true }),
      ]),
    );
  });

  it("requests a live catalog while Fast availability is unknown", () => {
    const state = codexFastSlashState(
      { ...session, codexFastMode: false, modelOptions: undefined },
      "",
      true,
      null,
    );

    expect(state.items).toEqual([]);
    expect(state.requiresLiveRefresh).toBe(true);
    expect(state.supportsLiveRefresh).toBe(true);
    expect(state.isRefreshing).toBe(true);
    expect(state.emptyMessage).toContain("Loading Fast availability");
    expect(state.hint).not.toBe(state.emptyMessage);
  });

  it("offers an explicit retry when the Fast capability lookup fails", () => {
    const state = codexFastSlashState(
      { ...session, codexFastMode: false, modelOptions: undefined },
      "",
      false,
      "Codex model list failed.",
    );

    expect(state.errorMessage).toBe("Codex model list failed.");
    expect(state.refreshActionLabel).toBe("Retry live models");
    expect(state.emptyMessage).toContain("could not be loaded");
  });

  it("explains authoritative lack of Fast support instead of hiding the command", () => {
    const state = codexFastSlashState(
      {
        ...session,
        codexFastMode: false,
        model: "gpt-5.3-codex-spark",
        modelOptions: [
          {
            value: "gpt-5.3-codex-spark",
            label: "GPT-5.3-Codex-Spark",
            serviceTiers: [],
          },
        ],
      },
      "",
    );

    expect(state.items).toEqual([]);
    expect(state.requiresLiveRefresh).toBe(false);
    expect(state.emptyMessage).toContain("Fast mode is not available");
    expect(state.hint).not.toBe(state.emptyMessage);
  });

  it("keeps Standard reachable while enabled Fast authority awaits a catalog", () => {
    const state = codexFastSlashState(
      { ...session, modelOptions: undefined },
      "",
    );

    expect(state.items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ value: "off", isCurrent: false }),
        expect.objectContaining({ value: "on", isCurrent: true }),
      ]),
    );
    expect(state.requiresLiveRefresh).toBe(true);
  });

  it("reports a query mismatch instead of masking it with unknown capability copy", () => {
    const state = codexFastSlashState(
      { ...session, modelOptions: undefined },
      "turbo",
    );

    expect(state.items).toEqual([]);
    expect(state.emptyMessage).toBe(
      'No Codex speed options match "turbo".',
    );
  });

  it("offers only Standard when the catalog no longer supports active Fast", () => {
    const state = codexFastSlashState(
      {
        ...session,
        modelOptions: [{ value: "gpt-5.5", label: "GPT-5.5", serviceTiers: [] }],
      },
      "",
    );

    expect(state.items).toEqual([
      expect.objectContaining({ value: "off", isCurrent: false }),
    ]);
  });

  it("reports a query mismatch instead of masking it with unsupported copy", () => {
    const state = codexFastSlashState(
      {
        ...session,
        modelOptions: [{ value: "gpt-5.5", label: "GPT-5.5", serviceTiers: [] }],
      },
      "turbo",
    );

    expect(state.items).toEqual([]);
    expect(state.emptyMessage).toBe(
      'No Codex speed options match "turbo".',
    );
  });

  it("settles an inconclusive loaded catalog into an explicit retry state", () => {
    const state = codexFastSlashState(
      {
        ...session,
        codexFastMode: false,
        modelOptions: [{ value: "other-model", label: "Other model" }],
      },
      "",
    );

    expect(state.emptyMessage).toContain("Codex did not report Fast availability");
    expect(state.emptyMessage).not.toContain("Fetching");
    expect(state.refreshActionLabel).toBe("Retry live models");
    expect(state.hint).not.toBe(state.emptyMessage);
  });
});

describe("Codex MCP slash status", () => {
  const session = {
    id: "session-codex",
    agent: "Codex",
    model: "gpt-5.6-sol",
    workdir: "/tmp",
  } as SlashPaletteSession;
  const servers = [
    {
      name: "termal",
      authStatus: "unsupported",
      tools: [
        {
          name: "termal_spawn_session",
          description: "Spawns a child session",
        },
      ],
    },
  ];

  it("exposes /mcp only for Codex sessions", () => {
    expect(slashCommandsForSession(session).map((command) => command.id)).toContain("mcp");
    expect(
      slashCommandsForSession({ ...session, agent: "Claude" }).map((command) => command.id),
    ).not.toContain("mcp");
  });

  it("shows compact and verbose MCP inventory states", () => {
    const compact = codexMcpSlashState(session, "", servers, "loaded", null);
    const verbose = codexMcpSlashState(session, "verbose", servers, "loaded", null);

    expect(compact.kind).toBe("mcp");
    expect(compact.verbose).toBe(false);
    expect(compact.servers).toEqual(servers);
    expect(compact.statusText).toBe("1 MCP server configured.");
    expect(verbose.verbose).toBe(true);
    expect(verbose.title).toContain("verbose");
  });

  it("shows usage without loading for unsupported arguments", () => {
    const state = codexMcpSlashState(session, "details", servers, "loaded", null);

    expect(state.supportsRefresh).toBe(false);
    expect(state.servers).toEqual([]);
    expect(state.emptyMessage).toBe("Usage: /mcp [verbose]");
  });

  it("surfaces loading and retryable errors", () => {
    const loading = codexMcpSlashState(session, "", [], "loading", null);
    const failed = codexMcpSlashState(
      session,
      "",
      [],
      "error",
      "Codex app-server unavailable",
    );

    expect(loading.isRefreshing).toBe(true);
    expect(loading.statusText).toContain("Loading");
    expect(failed.errorMessage).toBe("Codex app-server unavailable");
    expect(failed.refreshActionLabel).toBe("Retry MCP status");
  });
});

describe("OpenCode slash choices", () => {
  const session = {
    id: "session-opencode",
    agent: "OpenCode",
    agentCommandsRevision: 0,
    model: "openai/gpt-5.6-sol",
    opencodeModel: "auto",
    opencodeEffort: "high",
    opencodeCurrentEffort: "high",
    opencodeMode: "plan",
    opencodeCurrentMode: "plan",
    modelOptions: [
      { value: "openai/gpt-5.6-sol", label: "GPT-5.6 Sol" },
    ],
    opencodeModeOptions: [
      { value: "build", label: "Build" },
      { value: "plan", label: "Plan" },
    ],
    opencodeEffortOptions: [
      { value: "low", label: "Low" },
      { value: "high", label: "High" },
    ],
    workdir: "/tmp",
  } as SlashPaletteSession;

  it("exposes model, reasoning-variant, and mode commands", () => {
    expect(
      slashCommandsForSession(session).map((command) => command.id),
    ).toEqual(expect.arrayContaining(["model", "effort", "mode"]));
    expect(sessionModeSlashState(session, "")?.items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ value: "plan", isCurrent: true }),
      ]),
    );
  });

  it("uses the model-specific reasoning variants reported by OpenCode", () => {
    expect(opencodeEffortSlashState(session, "").items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ value: "auto", isCurrent: false }),
        expect.objectContaining({ value: "low", isCurrent: false }),
        expect.objectContaining({ value: "high", isCurrent: true }),
      ]),
    );
  });

  it("marks selected authority and never offers arbitrary manual models", () => {
    const state = sessionModelSlashState(
      session,
      "unlisted",
      "unlisted/provider",
      false,
      null,
    );
    expect(state.items).toEqual([]);

    const allModels = sessionModelSlashState(session, "", "", false, null);
    expect(allModels.items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ value: "auto", isCurrent: true }),
        expect.objectContaining({
          value: "openai/gpt-5.6-sol",
          isCurrent: false,
        }),
      ]),
    );
  });
});
