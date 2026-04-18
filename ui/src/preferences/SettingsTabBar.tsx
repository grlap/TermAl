// Horizontal tab bar at the top of the Settings dialog.
//
// What this file owns:
//   - The `<div role="tablist">` markup and the per-tab `<button>`
//     rendering.
//   - The click wiring from a tab to the `onSelectTab` callback.
//
// What this file does NOT own:
//   - The list of tab ids / labels (see `./preferences-tabs.ts`).
//   - The panel content rendered beneath the tab bar (stays in
//     `App.tsx` for now; will move in later splits).
//   - The active-tab state (still held by `App.tsx`; this component
//     is a controlled, stateless view).
//
// Split out of `ui/src/App.tsx` as the first UI extraction of the
// App.tsx -> preferences/* series. Behaviour-equivalent to the
// inline JSX it replaces — same classNames, same ARIA attributes,
// same element ids.

import { PREFERENCES_TABS, type PreferencesTabId } from "./preferences-tabs";

export function SettingsTabBar({
  activeTabId,
  onSelectTab,
}: {
  activeTabId: PreferencesTabId;
  onSelectTab: (id: PreferencesTabId) => void;
}) {
  return (
    <div
      className="settings-tab-list"
      role="tablist"
      aria-label="Preferences sections"
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
            onClick={() => onSelectTab(tab.id)}
          >
            {tab.label}
          </button>
        );
      })}
    </div>
  );
}
