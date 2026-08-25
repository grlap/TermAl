import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ThemeModeToggle } from "./workspace-shell-controls";

describe("ThemeModeToggle", () => {
  it("advertises and invokes the opposite effective theme", () => {
    const onToggle = vi.fn();
    render(
      <ThemeModeToggle
        effectiveThemeKind="dark"
        hasAutoOverride={false}
        onToggle={onToggle}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Switch to light theme" }));
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  it("marks a session-only Auto override", () => {
    render(
      <ThemeModeToggle
        effectiveThemeKind="light"
        hasAutoOverride
        onToggle={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Switch to dark theme" })).toHaveClass(
      "overridden",
    );
  });

  it("uses a shared grid row to match the workspace switcher height", async () => {
    const nodeFsModule = "node:fs";
    const { readFileSync } = (await import(nodeFsModule)) as {
      readFileSync: (path: string, encoding: "utf8") => string;
    };
    const runtimeProcess = (
      globalThis as typeof globalThis & {
        process: { cwd: () => string };
      }
    ).process;
    const styles = readFileSync(`${runtimeProcess.cwd()}/src/styles.css`, "utf8");
    const paneBarThemeRule = styles.match(
      /\.pane-bar-right\s+\.theme-mode-toggle\s*\{([^}]*)\}/,
    )?.[1];
    const paneBarRule = styles.match(/\.pane-bar-right\s*\{([^}]*)\}/)?.[1];
    const responsivePaneBarRule = [...styles.matchAll(/\.pane-bar-right\s*\{([^}]*)\}/g)]
      .map((match) => match[1])
      .find((rule) => /grid-template-columns:/.test(rule ?? ""));
    const responsivePaneRule = [...styles.matchAll(/\.pane-bar\s*\{([^}]*)\}/g)]
      .map((match) => match[1])
      .find((rule) => /grid-template-columns:/.test(rule ?? ""));

    expect(paneBarRule).toMatch(/display:\s*inline-grid\s*;/);
    expect(paneBarRule).toMatch(/grid-auto-flow:\s*column\s*;/);
    expect(paneBarRule).toMatch(/align-items:\s*stretch\s*;/);
    expect(responsivePaneBarRule).toMatch(
      /grid-template-columns:\s*max-content\s+minmax\(0,\s*1fr\)\s*;/,
    );
    expect(responsivePaneBarRule).toMatch(/width:\s*100%\s*;/);
    expect(responsivePaneRule).toMatch(
      /grid-template-columns:\s*minmax\(0,\s*1fr\)\s*;/,
    );
    expect(responsivePaneRule).toMatch(/justify-content:\s*stretch\s*;/);
    expect(paneBarThemeRule).toMatch(/width:\s*auto\s*;/);
    expect(paneBarThemeRule).toMatch(/height:\s*100%\s*;/);
    expect(paneBarThemeRule).toMatch(/padding:\s*0\s*;/);
    expect(paneBarThemeRule).toMatch(/aspect-ratio:\s*1\s*;/);
  });
});
