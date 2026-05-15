import { describe, expect, it } from "vitest";
import {
  markLiveSessionResumeWatchdogBaseline,
  markLiveTransportActivity,
  syncLiveSessionResumeWatchdogBaselines,
  syncLiveTransportActivityFromState,
} from "./app-live-state-activity";
import type { Session } from "./types";

function session(id: string): Session {
  return {
    id,
    name: id,
    emoji: "T",
    agent: "Codex",
    workdir: "/tmp/termal-test",
    model: "default",
    status: "idle",
    preview: "",
    messages: [],
  };
}

describe("app live-state activity helpers", () => {
  it("marks live transport activity for explicit and snapshot sessions", () => {
    const activity = new Map<string, number>();

    markLiveTransportActivity(activity, ["session-1"], 10);
    syncLiveTransportActivityFromState(
      activity,
      [session("session-2"), session("session-3")],
      20,
    );

    expect([...activity.entries()]).toEqual([
      ["session-1", 10],
      ["session-2", 20],
      ["session-3", 20],
    ]);
  });

  it("syncs resume-watchdog baselines and prunes missing sessions", () => {
    const baselines = new Map<string, number>([
      ["session-old", 5],
      ["session-keep", 6],
    ]);

    markLiveSessionResumeWatchdogBaseline(baselines, ["session-live"], 10);
    syncLiveSessionResumeWatchdogBaselines(
      baselines,
      [session("session-keep"), session("session-live")],
      20,
    );

    expect([...baselines.entries()]).toEqual([
      ["session-keep", 20],
      ["session-live", 20],
    ]);
  });
});
