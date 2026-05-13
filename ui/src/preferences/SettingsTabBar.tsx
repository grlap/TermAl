// Vertical tab rail in the Settings dialog.
//
// What this file owns:
//   - The `<div role="tablist">` markup and the per-tab `<button>`
//     rendering.
//   - The click wiring from a tab to the `onSelectTab` callback.
//   - The WAI-ARIA tablist keyboard pattern: roving `tabIndex`, and
//     arrow / Home / End keys that move the selection vertically
//     within the tablist without leaving it via `Tab`.
//
// What this file does NOT own:
//   - The list of tab ids / labels (see `./preferences-tabs.ts`).
//   - The panel content rendered next to the tab rail (stays in
//     `App.tsx` for now; will move in later splits).
//   - The active-tab state (still held by `App.tsx`; this component
//     is a controlled, stateless view).
//
// Split out of `ui/src/App.tsx` as the first UI extraction of the
// App.tsx -> preferences/* series. Behaviour-equivalent to the
// inline JSX it replaces — same classNames, same ARIA attributes,
// same element ids — plus the keyboard pattern that the inline
// tab rail never implemented.

import type { KeyboardEvent } from "react";

import { PREFERENCES_TABS, type PreferencesTabId } from "./preferences-tabs";

export function SettingsTabBar({
  activeTabId,
  onSelectTab,
}: {
  activeTabId: PreferencesTabId;
  onSelectTab: (id: PreferencesTabId) => void;
}) {
  // WAI-ARIA tablist keyboard pattern. Keys are handled on the
  // tablist wrapper rather than on each tab button so the focused
  // tab receives them regardless of which button holds focus at
  // the moment the key is pressed.
  //
  // Selection and focus are kept in sync: after picking the next
  // tab, we imperatively `.focus()` its button by id. The parent
  // re-renders with `tabIndex={0}` on that button, which keeps
  // roving-tabindex consistent with DOM focus. Calling `.focus()`
  // before the re-render is fine — `tabIndex={-1}` prevents entry
  // via `Tab`, not programmatic focus, and by the time the user
  // releases the key React has already committed the state update.
  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const key = event.key;
    const isPreviousKey = key === "ArrowUp" || key === "ArrowLeft";
    const isNextKey = key === "ArrowDown" || key === "ArrowRight";
    if (!isPreviousKey && !isNextKey && key !== "Home" && key !== "End") {
      return;
    }
    // Skip when a modifier is held so OS / browser shortcuts can
    // run unchanged — `Ctrl+ArrowRight` is commonly "jump word"
    // in inputs and "next browser tab" at the document level,
    // `Ctrl+Home` / `Ctrl+End` jump to document top/bottom,
    // and `Meta+ArrowLeft` is browser-back on macOS. The
    // WAI-ARIA tablist pattern specifies unmodified arrow keys
    // only; hijacking modifier combinations would surprise
    // users who reach for those shortcuts by reflex.
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return;
    }

    const currentIndex = PREFERENCES_TABS.findIndex(
      (tab) => tab.id === activeTabId,
    );
    if (currentIndex < 0) {
      return;
    }

    const count = PREFERENCES_TABS.length;
    let nextIndex = currentIndex;
    if (isPreviousKey) {
      nextIndex = (currentIndex - 1 + count) % count;
    } else if (isNextKey) {
      nextIndex = (currentIndex + 1) % count;
    } else if (key === "Home") {
      nextIndex = 0;
    } else if (key === "End") {
      nextIndex = count - 1;
    }

    // Stop the browser from scrolling on Home/End and from moving
    // focus on arrow keys outside the tablist.
    event.preventDefault();

    if (nextIndex === currentIndex) {
      // No-op destinations still count as handled tablist keys. Preventing the
      // default here keeps `Home` on the first tab and `End` on the last tab
      // from falling through to browser/document scrolling.
      return;
    }

    const nextTab = PREFERENCES_TABS[nextIndex];
    if (!nextTab) {
      return;
    }
    onSelectTab(nextTab.id);
    // Imperative focus so the user's keyboard position tracks the
    // selection. `settings-tab-${id}` is the stable id assigned
    // below; looking it up via `document.getElementById` avoids a
    // ref-array that would have to be manually kept in sync with
    // `PREFERENCES_TABS`.
    const nextButton = document.getElementById(`settings-tab-${nextTab.id}`);
    if (nextButton instanceof HTMLElement) {
      nextButton.focus();
    }
  }

  return (
    <div
      className="settings-tab-list"
      role="tablist"
      aria-label="Preferences sections"
      aria-orientation="vertical"
      onKeyDown={handleKeyDown}
    >
      {PREFERENCES_TABS.map((tab) => {
        const isSelected = activeTabId === tab.id;
        return (
          <button
            key={tab.id}
            id={`settings-tab-${tab.id}`}
            className={`settings-tab ${isSelected ? "selected" : ""}`}
            type="button"
            role="tab"
            aria-selected={isSelected}
            aria-controls={`settings-panel-${tab.id}`}
            // Roving tabindex: only the active tab participates in
            // sequential focus, so `Tab` leaves the tablist after
            // one stop instead of visiting every tab button.
            tabIndex={isSelected ? 0 : -1}
            onClick={() => onSelectTab(tab.id)}
          >
            {tab.label}
          </button>
        );
      })}
    </div>
  );
}
