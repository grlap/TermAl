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

describe("remote marker placement CSS", () => {
  // Resolved beside this file, not from the working directory, so the test
  // reads the same stylesheet whichever directory Vitest is started from.
  async function readStyles() {
    const nodeFsModule = "node:fs";
    const nodeUrlModule = "node:url";
    const { readFileSync } = (await import(nodeFsModule)) as {
      readFileSync: (path: string, encoding: "utf8") => string;
    };
    const { fileURLToPath } = (await import(nodeUrlModule)) as { fileURLToPath: (url: string) => string };
    const moduleUrl = import.meta.url;
    return readFileSync(fileURLToPath(new URL("./styles.css", moduleUrl).href), "utf8");
  }

  function rule(styles: string, selector: RegExp) {
    const bodies = [...styles.matchAll(new RegExp(`(?:^|\\n)${selector.source}\\s*\\{([^}]*)\\}`, "g"))]
      .map((match) => match[1]);
    const body = bodies[bodies.length - 1];
    if (body === undefined) throw new Error(`No rule for ${selector.source}`);
    return body;
  }

  it("sits immediately left of the agent-icon status button, aligned with it", async () => {
    const styles = await readStyles();
    const statusShared = rule(styles, /\.session-row-status-button,\s*\.session-row-kill/);
    const statusButton = rule(styles, /\.session-row-status-button/);
    const marker = rule(styles, /\.session-row-remote-marker/);
    // The status button: 0.8rem from the tile's top right, 2.1rem square.
    expect(statusShared).toMatch(/top:\s*0\.8rem;/);
    expect(statusShared).toMatch(/right:\s*0\.8rem;/);
    expect(statusButton).toMatch(/width:\s*2\.1rem;/);
    expect(statusButton).toMatch(/height:\s*2\.1rem;/);
    // The marker: same top and height, right edge just past the button's left edge.
    expect(marker).toMatch(/position:\s*absolute;/);
    expect(marker).toMatch(/top:\s*0\.8rem;/);
    expect(marker).toMatch(/height:\s*2\.1rem;/);
    expect(marker).toMatch(/right:\s*calc\(0\.8rem \+ 2\.1rem \+ 0\.3rem\);/);
  });

  it("reserves room for the marker only on remote tiles and hides it while Kill is shown", async () => {
    const styles = await readStyles();
    expect(rule(styles, /\.session-title-line/)).toMatch(/padding-right:\s*2\.9rem;/);
    expect(rule(styles, /\.session-row-shell\.remote \.session-title-line/))
      .toMatch(/padding-right:\s*calc\(2\.9rem \+ var\(--icon-size-sm\) \+ 0\.3rem\);/);
    expect(rule(styles, /\.session-row-shell\.kill-armed \.session-row-remote-marker/)).toMatch(/opacity:\s*0;/);
  });
});
