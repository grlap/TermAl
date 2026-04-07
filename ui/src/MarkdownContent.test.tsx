import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MarkdownContent } from "./App";

describe("MarkdownContent inline file links", () => {
  it("renders inline code file references as clickable links when the source callback exists", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="Text like `experience.tex.#L63` should stay clickable."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const link = screen.getByRole("link", { name: "experience.tex.#L63" });
    expect(link).toHaveClass("inline-code-link");
    expect(link).toHaveAttribute("draggable", "false");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("renders inline code file references as plain code without the source callback", () => {
    render(
      <MarkdownContent
        markdown="Text like `experience.tex.#L63` should stay plain code."
        workspaceRoot="/repo"
      />,
    );

    expect(
      screen.queryByRole("link", { name: "experience.tex.#L63" }),
    ).toBeNull();
    expect(
      screen.getByText("experience.tex.#L63", { selector: "code" }).closest("a"),
    ).toBeNull();
  });

  it("preserves inline code link DOM nodes when the source callback identity changes", () => {
    const firstOnOpenSourceLink = vi.fn();
    const secondOnOpenSourceLink = vi.fn();
    const markdown = "Text like `experience.tex.#L63` should stay clickable.";
    const { rerender } = render(
      <MarkdownContent
        markdown={markdown}
        onOpenSourceLink={firstOnOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const firstLink = screen.getByRole("link", { name: "experience.tex.#L63" });
    const firstCode = firstLink.querySelector("code");

    rerender(
      <MarkdownContent
        markdown={markdown}
        onOpenSourceLink={secondOnOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const secondLink = screen.getByRole("link", { name: "experience.tex.#L63" });
    expect(secondLink).toBe(firstLink);
    expect(secondLink.querySelector("code")).toBe(firstCode);

    fireEvent.click(secondLink);

    expect(firstOnOpenSourceLink).not.toHaveBeenCalled();
    expect(secondOnOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });
});
