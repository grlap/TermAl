import { afterEach, describe, expect, it, vi } from "vitest";

import { detectBrowserPlatform, isApplePlatform } from "./browser-platform";

describe("browser platform detection", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("prefers userAgentData and falls back to navigator.platform", () => {
    vi.stubGlobal("navigator", {
      platform: "Win32",
      userAgentData: { platform: "macOS" },
    });
    expect(detectBrowserPlatform()).toBe("macOS");

    vi.stubGlobal("navigator", { platform: "Linux x86_64" });
    expect(detectBrowserPlatform()).toBe("Linux x86_64");
  });

  it("returns an empty platform when browser metadata is unavailable", () => {
    vi.stubGlobal("navigator", {});
    expect(detectBrowserPlatform()).toBe("");

    vi.stubGlobal("navigator", undefined);
    expect(detectBrowserPlatform()).toBe("");
  });

  it.each([
    ["macOS", true],
    ["MacIntel", true],
    ["iPhone", true],
    ["Win32", false],
    ["Linux x86_64", false],
  ])("classifies %s as Apple=%s", (platform, expected) => {
    expect(isApplePlatform(platform)).toBe(expected);
  });
});
