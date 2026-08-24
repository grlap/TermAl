// Owns Response Board inner-tab navigation and tab-management controls.
// Deliberately does not own tab persistence, board loading, or canvas state.
// Split from ResponseBoardPanel.tsx.

import type { RefObject } from "react";

import {
  RESPONSE_BOARD_DEFAULT_TAB_ID,
  type ResponseBoardTab,
} from "../api";

export function ResponseBoardTabStrip({
  tabs,
  workspaceTabId,
  selectedTabId,
  renamingTabId,
  renameValue,
  isAddingTab,
  newTabName,
  isReorderingTabs,
  tabButtonRefs,
  onSelectTab,
  onRenameValueChange,
  onCancelRename,
  onSubmitRename,
  onStartRename,
  onReorderTab,
  onDeleteTab,
  onNewTabNameChange,
  onCancelAdd,
  onSubmitAdd,
  onStartAdd,
}: {
  tabs: ResponseBoardTab[];
  workspaceTabId: string;
  selectedTabId: string | null;
  renamingTabId: string | null;
  renameValue: string;
  isAddingTab: boolean;
  newTabName: string;
  isReorderingTabs: boolean;
  tabButtonRefs: RefObject<Map<string, HTMLButtonElement>>;
  onSelectTab: (tabId: string) => void;
  onRenameValueChange: (value: string) => void;
  onCancelRename: () => void;
  onSubmitRename: (tabId: string) => void;
  onStartRename: (tab: ResponseBoardTab) => void;
  onReorderTab: (tabId: string, offset: -1 | 1) => void;
  onDeleteTab: (tab: ResponseBoardTab) => void;
  onNewTabNameChange: (value: string) => void;
  onCancelAdd: () => void;
  onSubmitAdd: () => void;
  onStartAdd: () => void;
}) {
  return (
    <nav className="response-board-tabs" aria-label="Response board tabs">
      <div className="response-board-tab-list" role="tablist">
        {tabs.map((tab) =>
          renamingTabId === tab.id ? (
            <form
              key={tab.id}
              className="response-board-tab-edit"
              role="presentation"
              onSubmit={(event) => {
                event.preventDefault();
                onSubmitRename(tab.id);
              }}
            >
              <input
                autoFocus
                value={renameValue}
                aria-label="Rename board tab"
                onChange={(event) => onRenameValueChange(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    onCancelRename();
                  }
                }}
              />
            </form>
          ) : (
            <div
              key={tab.id}
              className="response-board-tab-wrap"
              role="presentation"
            >
              <button
                type="button"
                role="tab"
                id={`response-board-tab-${workspaceTabId}-${tab.id}`}
                aria-controls={`response-board-tabpanel-${workspaceTabId}-${tab.id}`}
                tabIndex={tab.id === selectedTabId ? 0 : -1}
                aria-selected={tab.id === selectedTabId}
                className={tab.id === selectedTabId ? "is-active" : ""}
                ref={(node) => {
                  if (node) {
                    tabButtonRefs.current?.set(tab.id, node);
                  } else {
                    tabButtonRefs.current?.delete(tab.id);
                  }
                }}
                onClick={() => onSelectTab(tab.id)}
                onKeyDown={(event) => {
                  const currentIndex = tabs.findIndex(
                    (candidate) => candidate.id === tab.id,
                  );
                  const targetIndex =
                    event.key === "ArrowRight"
                      ? Math.min(tabs.length - 1, currentIndex + 1)
                      : event.key === "ArrowLeft"
                        ? Math.max(0, currentIndex - 1)
                        : event.key === "Home"
                          ? 0
                          : event.key === "End"
                            ? tabs.length - 1
                            : null;
                  if (targetIndex === null || targetIndex === currentIndex) {
                    return;
                  }
                  event.preventDefault();
                  const target = tabs[targetIndex];
                  onSelectTab(target.id);
                  tabButtonRefs.current?.get(target.id)?.focus();
                }}
                onDoubleClick={() => {
                  if (tab.kind === "custom") {
                    onStartRename(tab);
                  }
                }}
              >
                <span>{tab.name}</span>
                <small>{tab.placedCardCount}</small>
              </button>
              {tab.id === selectedTabId && tab.kind === "custom" ? (
                <div className="response-board-tab-actions">
                  <button
                    type="button"
                    aria-label={`Move ${tab.name} left`}
                    disabled={isReorderingTabs || tabs[0]?.id === tab.id}
                    onClick={() => onReorderTab(tab.id, -1)}
                  >
                    ‹
                  </button>
                  <button
                    type="button"
                    aria-label={`Move ${tab.name} right`}
                    disabled={
                      isReorderingTabs || tabs[tabs.length - 1]?.id === tab.id
                    }
                    onClick={() => onReorderTab(tab.id, 1)}
                  >
                    ›
                  </button>
                  <button
                    type="button"
                    aria-label={`Rename ${tab.name}`}
                    onClick={() => onStartRename(tab)}
                  >
                    ✎
                  </button>
                  {tab.id !== RESPONSE_BOARD_DEFAULT_TAB_ID ? (
                    <button
                      type="button"
                      aria-label={`Delete ${tab.name}`}
                      disabled={tab.placedCardCount > 0}
                      onClick={() => onDeleteTab(tab)}
                    >
                      ×
                    </button>
                  ) : null}
                </div>
              ) : null}
            </div>
          ),
        )}
        {isAddingTab ? (
          <form
            className="response-board-tab-edit"
            onSubmit={(event) => {
              event.preventDefault();
              onSubmitAdd();
            }}
          >
            <input
              autoFocus
              value={newTabName}
              aria-label="New board tab name"
              placeholder="Tab name"
              onChange={(event) => onNewTabNameChange(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  onCancelAdd();
                }
              }}
            />
          </form>
        ) : (
          <button
            type="button"
            className="response-board-add-tab"
            aria-label="Add response board tab"
            onClick={onStartAdd}
          >
            +
          </button>
        )}
      </div>
    </nav>
  );
}
