// Tests for the Settings dialog tab rail's WAI-ARIA tablist
// keyboard pattern.
//
// Pins the roving tabindex contract (only the active tab
// participates in sequential focus) and the arrow / Home / End
// key handlers that move the selection vertically within the
// tablist. The inline tab-bar JSX this component replaced had
// neither — click / Enter / Space were the only selection
// mechanisms — so these tests are both regression guards and
// the first direct coverage of the keyboard pattern.

import { describe, expect, it, vi } from "vitest";
import { createEvent, fireEvent, render, screen } from "@testing-library/react";

import { SettingsTabBar } from "./SettingsTabBar";
import { PREFERENCES_TABS } from "./preferences-tabs";

describe("SettingsTabBar keyboard navigation", () => {
  it("puts only the active tab in sequential Tab order", () => {
    render(<SettingsTabBar activeTabId="themes" onSelectTab={() => {}} />);
    expect(screen.getByRole("tablist")).toHaveAttribute(
      "aria-orientation",
      "vertical",
    );
    const themes = screen.getByRole("tab", { name: "Themes" });
    const markdown = screen.getByRole("tab", { name: "Markdown" });
    // Active tab participates in `Tab` traversal (tabIndex=0);
    // every other tab is focusable via .focus() but skipped by
    // `Tab` (tabIndex=-1). This is the roving-tabindex contract.
    expect(themes).toHaveAttribute("tabindex", "0");
    expect(markdown).toHaveAttribute("tabindex", "-1");
  });

  it("moves selection to the next tab on ArrowDown and focuses it", () => {
    const onSelectTab = vi.fn();
    render(
      <SettingsTabBar activeTabId="themes" onSelectTab={onSelectTab} />,
    );
    const themes = screen.getByRole("tab", { name: "Themes" });
    themes.focus();
    fireEvent.keyDown(themes, { key: "ArrowDown" });
    // Selection advances to the next tab in PREFERENCES_TABS.
    expect(onSelectTab).toHaveBeenCalledWith("markdown");
    // DOM focus moves in lockstep — otherwise the user sees
    // selection on one tab and focus on another, a WAI-ARIA
    // tablist protocol violation.
    expect(document.activeElement).toBe(
      screen.getByRole("tab", { name: "Markdown" }),
    );
  });

  it("moves selection to the previous tab on ArrowUp and focuses it", () => {
    const onSelectTab = vi.fn();
    render(
      <SettingsTabBar activeTabId="markdown" onSelectTab={onSelectTab} />,
    );
    const markdown = screen.getByRole("tab", { name: "Markdown" });
    markdown.focus();
    fireEvent.keyDown(markdown, { key: "ArrowUp" });
    expect(onSelectTab).toHaveBeenCalledWith("themes");
    expect(document.activeElement).toBe(
      screen.getByRole("tab", { name: "Themes" }),
    );
  });

  it("wraps to the last tab (and focuses it) when pressing ArrowUp on the first", () => {
    const onSelectTab = vi.fn();
    render(<SettingsTabBar activeTabId="themes" onSelectTab={onSelectTab} />);
    const themes = screen.getByRole("tab", { name: "Themes" });
    themes.focus();
    fireEvent.keyDown(themes, { key: "ArrowUp" });
    const lastTab = PREFERENCES_TABS[PREFERENCES_TABS.length - 1];
    expect(lastTab).toBeDefined();
    if (!lastTab) {
      return;
    }
    expect(onSelectTab).toHaveBeenCalledWith(lastTab.id);
    // Focus must also wrap; a wrap-branch bug that computes the
    // right id for `onSelectTab` but the wrong id for
    // `document.getElementById` would slip past a
    // selection-only assertion.
    expect(document.activeElement).toBe(
      screen.getByRole("tab", { name: lastTab.label }),
    );
  });

  it("wraps to the first tab (and focuses it) when pressing ArrowDown on the last", () => {
    const onSelectTab = vi.fn();
    const lastTab = PREFERENCES_TABS[PREFERENCES_TABS.length - 1];
    const firstTab = PREFERENCES_TABS[0];
    expect(lastTab).toBeDefined();
    expect(firstTab).toBeDefined();
    if (!lastTab || !firstTab) {
      return;
    }
    render(
      <SettingsTabBar activeTabId={lastTab.id} onSelectTab={onSelectTab} />,
    );
    const tabs = screen.getAllByRole("tab");
    const lastTabElement = tabs[tabs.length - 1];
    expect(lastTabElement).toBeDefined();
    if (!lastTabElement) {
      return;
    }
    lastTabElement.focus();
    fireEvent.keyDown(lastTabElement, { key: "ArrowDown" });
    expect(onSelectTab).toHaveBeenCalledWith(firstTab.id);
    expect(document.activeElement).toBe(
      screen.getByRole("tab", { name: firstTab.label }),
    );
  });

  it("jumps to the first tab on Home and focuses it", () => {
    const onSelectTab = vi.fn();
    render(
      <SettingsTabBar activeTabId="markdown" onSelectTab={onSelectTab} />,
    );
    const markdown = screen.getByRole("tab", { name: "Markdown" });
    markdown.focus();
    fireEvent.keyDown(markdown, { key: "Home" });
    const firstTab = PREFERENCES_TABS[0];
    expect(firstTab).toBeDefined();
    if (!firstTab) {
      return;
    }
    // Exactly one call; an implementation that walked
    // arrow-style to index 0 would fire `onSelectTab` for
    // every intermediate index and slip past a
    // `toHaveBeenLastCalledWith` check.
    expect(onSelectTab).toHaveBeenCalledTimes(1);
    expect(onSelectTab).toHaveBeenCalledWith(firstTab.id);
    expect(document.activeElement).toBe(
      screen.getByRole("tab", { name: firstTab.label }),
    );
  });

  it("prevents default on Home when the first tab is already active", () => {
    const onSelectTab = vi.fn();
    render(<SettingsTabBar activeTabId="themes" onSelectTab={onSelectTab} />);
    const themes = screen.getByRole("tab", { name: "Themes" });
    themes.focus();
    const homeEvent = createEvent.keyDown(themes, { key: "Home" });
    fireEvent(themes, homeEvent);
    expect(homeEvent.defaultPrevented).toBe(true);
    expect(onSelectTab).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(themes);
  });

  it("jumps to the last tab on End and focuses it", () => {
    const onSelectTab = vi.fn();
    render(
      <SettingsTabBar activeTabId="markdown" onSelectTab={onSelectTab} />,
    );
    const markdown = screen.getByRole("tab", { name: "Markdown" });
    markdown.focus();
    fireEvent.keyDown(markdown, { key: "End" });
    const lastTab = PREFERENCES_TABS[PREFERENCES_TABS.length - 1];
    expect(lastTab).toBeDefined();
    if (!lastTab) {
      return;
    }
    expect(onSelectTab).toHaveBeenCalledTimes(1);
    expect(onSelectTab).toHaveBeenCalledWith(lastTab.id);
    expect(document.activeElement).toBe(
      screen.getByRole("tab", { name: lastTab.label }),
    );
  });

  it("prevents default on End when the last tab is already active", () => {
    const onSelectTab = vi.fn();
    const lastTab = PREFERENCES_TABS[PREFERENCES_TABS.length - 1];
    expect(lastTab).toBeDefined();
    if (!lastTab) {
      return;
    }
    render(<SettingsTabBar activeTabId={lastTab.id} onSelectTab={onSelectTab} />);
    const lastTabElement = screen.getByRole("tab", { name: lastTab.label });
    lastTabElement.focus();
    const endEvent = createEvent.keyDown(lastTabElement, { key: "End" });
    fireEvent(lastTabElement, endEvent);
    expect(endEvent.defaultPrevented).toBe(true);
    expect(onSelectTab).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(lastTabElement);
  });

  it("ignores unrelated keys without calling onSelectTab", () => {
    const onSelectTab = vi.fn();
    render(<SettingsTabBar activeTabId="themes" onSelectTab={onSelectTab} />);
    const themes = screen.getByRole("tab", { name: "Themes" });
    themes.focus();
    // `Tab`, typing, and `Escape` should all pass through
    // without triggering tab selection.
    fireEvent.keyDown(themes, { key: "Tab" });
    fireEvent.keyDown(themes, { key: "a" });
    fireEvent.keyDown(themes, { key: "Escape" });
    expect(onSelectTab).not.toHaveBeenCalled();
  });

  it("ignores modifier-arrow combinations so OS/browser shortcuts work", () => {
    // Ctrl+ArrowDown / Ctrl+ArrowRight are browser or editor shortcuts,
    // Meta+ArrowUp / Meta+ArrowLeft are browser or document shortcuts,
    // Ctrl+End jump to document top/bottom. Intercepting these
    // would surprise users who reach for them by reflex, and
    // WAI-ARIA's tablist pattern specifies unmodified arrow
    // keys only.
    const onSelectTab = vi.fn();
    render(<SettingsTabBar activeTabId="themes" onSelectTab={onSelectTab} />);
    const themes = screen.getByRole("tab", { name: "Themes" });
    themes.focus();
    fireEvent.keyDown(themes, { key: "ArrowDown", ctrlKey: true });
    fireEvent.keyDown(themes, { key: "ArrowUp", metaKey: true });
    fireEvent.keyDown(themes, { key: "Home", altKey: true });
    fireEvent.keyDown(themes, { key: "End", ctrlKey: true });
    expect(onSelectTab).not.toHaveBeenCalled();
  });

  it("preserves click selection unchanged by the keyboard handler", () => {
    const onSelectTab = vi.fn();
    render(<SettingsTabBar activeTabId="themes" onSelectTab={onSelectTab} />);
    fireEvent.click(screen.getByRole("tab", { name: "Remotes" }));
    expect(onSelectTab).toHaveBeenCalledWith("remotes");
  });
});
