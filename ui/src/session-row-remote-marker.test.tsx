// Pins the remote marker of a session list tile: which sessions get one, its
// accessible wording, and the CSS rules that place it immediately left of the
// tile's agent-icon status button. The App wiring is covered in
// App.session-remote-marker.test.tsx.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SessionRowRemoteMarker, describeSessionRemote } from "./session-row-remote-marker";
import type { RemoteConfig } from "./types";

const macStudio: RemoteConfig = {
  id: "mac-studio",
  name: "Mac Studio",
  transport: "ssh",
  enabled: true,
  host: "mac.local",
  port: 2222,
  user: "greg",
};
const remotes = new Map<string, RemoteConfig>([["mac-studio", macStudio]]);

describe("describeSessionRemote", () => {
  it("returns nothing for a local session", () => {
    expect(describeSessionRemote({}, null, remotes)).toBeNull();
    expect(describeSessionRemote({}, { remoteId: "local" }, remotes)).toBeNull();
    expect(describeSessionRemote({}, { remoteId: null }, remotes)).toBeNull();
  });

  it("names the remote and its host for a remote session", () => {
    expect(describeSessionRemote({ remoteId: "mac-studio" }, null, remotes)).toEqual({
      remoteId: "mac-studio",
      label: "Remote session on Mac Studio (greg@mac.local:2222)",
    });
  });

  it("takes the project's remote when the session names none", () => {
    expect(describeSessionRemote({}, { remoteId: "mac-studio" }, remotes)?.remoteId).toBe("mac-studio");
  });

  it("prefers the session's own remote over its project's", () => {
    expect(describeSessionRemote({ remoteId: "mac-studio" }, { remoteId: "local" }, remotes)?.remoteId)
      .toBe("mac-studio");
  });

  it("falls back to the remote id when no remote config is known", () => {
    expect(describeSessionRemote({ remoteId: "gone" }, null, remotes)).toEqual({
      remoteId: "gone",
      label: "Remote session on gone",
    });
  });
});

describe("SessionRowRemoteMarker", () => {
  it("renders a labelled image with a tooltip and a decorative glyph", () => {
    render(<SessionRowRemoteMarker remote={{ remoteId: "mac-studio", label: "Remote session on Mac Studio" }} />);
    const marker = screen.getByRole("img", { name: "Remote session on Mac Studio" });
    expect(marker).toHaveClass("session-row-remote-marker");
    expect(marker).toHaveAttribute("title", "Remote session on Mac Studio");
    expect(marker.querySelector("svg")).toHaveAttribute("aria-hidden", "true");
  });
});
