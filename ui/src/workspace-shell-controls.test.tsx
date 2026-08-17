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
});
