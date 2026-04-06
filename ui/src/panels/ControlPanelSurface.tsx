import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
  type ReactNode,
} from "react";
import { GitHubMark } from "../github-mark";
import {
  TAB_DRAG_MIME_TYPE,
  attachWorkspaceTabDragData,
  createWorkspaceTabDrag,
  type WorkspaceTabDrag,
} from "../tab-drag";
import type { WorkspaceTab } from "../workspace";

export type ControlPanelSectionId = "files" | "sessions" | "projects" | "orchestrators" | "git";

const DEFAULT_CONTROL_PANEL_SECTION_ORDER: readonly ControlPanelSectionId[] = [
  "projects",
  "sessions",
  "orchestrators",
  "files",
  "git",
];
const CONTROL_PANEL_SECTION_ORDER_STORAGE_KEY = "termal-control-panel-section-order-v2";

type ControlPanelSurfaceProps = {
  fixedSection?: ControlPanelSectionId | null;
  gitStatusCount: number;
  isPreferencesOpen: boolean;
  onOpenPreferences: () => void;
  onSectionTabDragEnd?: () => void;
  onSectionTabDragStart?: (
    event: ReactDragEvent<HTMLButtonElement>,
    sectionId: ControlPanelSectionId,
  ) => void;
  projectCount: number;
  renderHeaderActions?: (sectionId: ControlPanelSectionId) => ReactNode;
  renderSection: (sectionId: ControlPanelSectionId) => ReactNode;
  sectionLauncherTabs?: Partial<Record<ControlPanelSectionId, WorkspaceTab | null>>;
  sessionCount: number;
  windowId?: string;
  launcherPaneId?: string;
};

export type ControlPanelSurfaceHandle = {
  selectSection: (sectionId: ControlPanelSectionId) => void;
};

type ControlPanelSectionDefinition = {
  badgeCount?: number;
  icon: ReactNode;
  id: ControlPanelSectionId;
  label: string;
};

type ControlPanelActionDefinition = {
  icon: ReactNode;
  label: string;
};

type DockDropPosition = "before" | "after";

type DockDropTarget = {
  position: DockDropPosition;
  sectionId: ControlPanelSectionId;
};

const PREFERENCES_ACTION: ControlPanelActionDefinition = {
  label: "Open preferences",
  icon: <SettingsIcon />,
};

export function ControlPanelSectionIcon({ sectionId }: { sectionId: ControlPanelSectionId }) {
  switch (sectionId) {
    case "files":
      return <FilesIcon />;
    case "sessions":
      return <SessionsIcon />;
    case "projects":
      return <ProjectsIcon />;
    case "orchestrators":
      return <OrchestratorsIcon />;
    case "git":
      return <GitStatusIcon />;
  }
}

export const ControlPanelSurface = forwardRef<ControlPanelSurfaceHandle, ControlPanelSurfaceProps>(function ControlPanelSurface({
  fixedSection = null,
  gitStatusCount,
  isPreferencesOpen,
  onOpenPreferences,
  onSectionTabDragEnd,
  onSectionTabDragStart,
  projectCount,
  renderHeaderActions,
  renderSection,
  sectionLauncherTabs,
  sessionCount,
  windowId,
  launcherPaneId,
}, ref): JSX.Element {
  const [activeSection, setActiveSection] = useState<ControlPanelSectionId>(fixedSection ?? "sessions");
  const [sectionOrder, setSectionOrder] = useState<ControlPanelSectionId[]>(() => getStoredControlPanelSectionOrder());
  const [draggedSectionId, setDraggedSectionId] = useState<ControlPanelSectionId | null>(null);
  const [dropTarget, setDropTarget] = useState<DockDropTarget | null>(null);
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const sectionDefinitionLookup: Record<ControlPanelSectionId, ControlPanelSectionDefinition> = {
    files: {
      id: "files",
      label: "Files",
      icon: <ControlPanelSectionIcon sectionId="files" />,
    },
    sessions: {
      badgeCount: sessionCount,
      id: "sessions",
      label: "Sessions",
      icon: <ControlPanelSectionIcon sectionId="sessions" />,
    },
    projects: {
      badgeCount: projectCount,
      id: "projects",
      label: "Projects",
      icon: <ControlPanelSectionIcon sectionId="projects" />,
    },
    orchestrators: {
      id: "orchestrators",
      label: "Orchestrators",
      icon: <ControlPanelSectionIcon sectionId="orchestrators" />,
    },
    git: {
      badgeCount: gitStatusCount,
      id: "git",
      label: "Git status",
      icon: <ControlPanelSectionIcon sectionId="git" />,
    },
  };
  const sectionDefinitions = fixedSection
    ? [sectionDefinitionLookup[fixedSection]]
    : sectionOrder.map((sectionId) => sectionDefinitionLookup[sectionId]);
  const activeSectionDefinition = fixedSection
    ? sectionDefinitionLookup[fixedSection]
    : (sectionDefinitions.find((definition) => definition.id === activeSection) ?? sectionDefinitions[0]);
  const headerActions = renderHeaderActions?.(activeSectionDefinition.id) ?? null;

  useImperativeHandle(ref, () => ({
    selectSection(sectionId) {
      if (fixedSection) {
        return;
      }
      setActiveSection(sectionId);
    },
  }), [fixedSection]);

  useEffect(() => {
    if (fixedSection) {
      return;
    }

    persistControlPanelSectionOrder(sectionOrder);
  }, [fixedSection, sectionOrder]);

  useEffect(() => {
    if (!fixedSection) {
      return;
    }

    setActiveSection(fixedSection);
  }, [fixedSection]);

  useEffect(() => {
    if (bodyRef.current) {
      bodyRef.current.scrollTop = 0;
    }
  }, [activeSection]);

  function clearDragState() {
    setDraggedSectionId(null);
    setDropTarget(null);
  }

  function handleSectionDragStart(event: ReactDragEvent<HTMLButtonElement>, sectionId: ControlPanelSectionId) {
    setDraggedSectionId(sectionId);
    setDropTarget(null);
    event.dataTransfer?.setData("text/plain", sectionId);
    if (event.dataTransfer) {
      event.dataTransfer.effectAllowed = "move";
    }
  }

  function handleSectionDragOver(event: ReactDragEvent<HTMLButtonElement>, sectionId: ControlPanelSectionId) {
    if (!draggedSectionId || draggedSectionId === sectionId) {
      setDropTarget(null);
      return;
    }

    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = "move";
    }

    const position = resolveDockDropPosition(event);
    setDropTarget((current) =>
      current?.sectionId === sectionId && current.position === position
        ? current
        : { sectionId, position },
    );
  }

  function handleSectionDrop(event: ReactDragEvent<HTMLButtonElement>, sectionId: ControlPanelSectionId) {
    if (!draggedSectionId) {
      clearDragState();
      return;
    }

    event.preventDefault();
    const position = resolveDockDropPosition(event);
    setSectionOrder((current) => moveSectionOrder(current, draggedSectionId, sectionId, position));
    clearDragState();
  }

  return (
    <div className={`control-panel-shell${fixedSection ? " fixed-section" : ""}`}>
      {fixedSection ? null : (
        <nav className="control-panel-activity-rail" aria-label="Control panel dock">
          <div className="control-panel-activity-group">
            {sectionDefinitions.map((definition) => (
              <ControlPanelActivityButton
                key={definition.id}
                definition={definition}
                dropPosition={dropTarget?.sectionId === definition.id ? dropTarget.position : null}
                isActive={activeSection === definition.id}
                isDragging={draggedSectionId === definition.id}
                launcherTab={sectionLauncherTabs?.[definition.id] ?? null}
                windowId={windowId}
                launcherPaneId={launcherPaneId}
                onExternalDragEnd={onSectionTabDragEnd}
                onExternalDragStart={onSectionTabDragStart}
                onDragEnd={clearDragState}
                onDragOver={handleSectionDragOver}
                onDragStart={handleSectionDragStart}
                onDrop={handleSectionDrop}
                onSelect={setActiveSection}
              />
            ))}
          </div>
          <div className="control-panel-activity-spacer" />
          <div className="control-panel-activity-group">
            <ControlPanelActionButton
              definition={PREFERENCES_ACTION}
              isExpanded={isPreferencesOpen}
              onClick={onOpenPreferences}
            />
          </div>
        </nav>
      )}

      <section className="control-panel-content">
        <header className="control-panel-header">
          <div className="control-panel-header-row">
            <h2>{activeSectionDefinition.label}</h2>
            {headerActions ? <div className="control-panel-header-actions">{headerActions}</div> : null}
          </div>
        </header>
        <div ref={bodyRef} className="control-panel-body" data-section={activeSection}>
          {renderSection(activeSection)}
        </div>
      </section>
    </div>
  );
});

ControlPanelSurface.displayName = "ControlPanelSurface";

function ControlPanelActivityButton({
  definition,
  dropPosition,
  isActive,
  isDragging,
  launcherTab,
  windowId,
  launcherPaneId,
  onExternalDragEnd,
  onExternalDragStart,
  onDragEnd,
  onDragOver,
  onDragStart,
  onDrop,
  onSelect,
}: {
  definition: ControlPanelSectionDefinition;
  dropPosition: DockDropPosition | null;
  isActive: boolean;
  isDragging: boolean;
  launcherTab: WorkspaceTab | null;
  windowId?: string;
  launcherPaneId?: string;
  onExternalDragEnd?: () => void;
  onExternalDragStart?: (
    event: ReactDragEvent<HTMLButtonElement>,
    sectionId: ControlPanelSectionId,
  ) => void;
  onDragEnd: () => void;
  onDragOver: (event: ReactDragEvent<HTMLButtonElement>, sectionId: ControlPanelSectionId) => void;
  onDragStart: (event: ReactDragEvent<HTMLButtonElement>, sectionId: ControlPanelSectionId) => void;
  onDrop: (event: ReactDragEvent<HTMLButtonElement>, sectionId: ControlPanelSectionId) => void;
  onSelect: (sectionId: ControlPanelSectionId) => void;
}) {
  const showBadge = Number.isFinite(definition.badgeCount) && (definition.badgeCount ?? 0) > 0;
  const renderedBadge = showBadge ? Math.min(definition.badgeCount ?? 0, 99) : null;

  return (
    <button
      className={`control-panel-activity-button control-panel-section-button${isActive ? " selected" : ""}${isDragging ? " dragging" : ""}${dropPosition ? ` drop-${dropPosition}` : ""}`}
      type="button"
      draggable
      aria-label={definition.label}
      aria-pressed={isActive}
      title={`${definition.label} (drag to open as tab, Shift+drag to reorder)`}
      onClick={() => onSelect(definition.id)}
      onDragEnd={() => {
        onDragEnd();
        onExternalDragEnd?.();
      }}
      onDragOver={(event) => onDragOver(event, definition.id)}
      onDragStart={(event) => {
        if (event.shiftKey || !onExternalDragStart) {
          onDragStart(event, definition.id);
        } else {
          // Attach drag data directly on the dataTransfer as a guarantee — the
          // callback chain through onExternalDragStart also sets it, but this
          // ensures the MIME type is present even if the callback fails to run.
          if (launcherTab && windowId && launcherPaneId) {
            const drag = createWorkspaceTabDrag(
              windowId,
              `control-panel-launcher:${launcherPaneId}:${definition.id}`,
              launcherTab,
            );
            event.dataTransfer.effectAllowed = "copyMove";
            attachWorkspaceTabDragData(event.dataTransfer, drag);
          }
          onExternalDragStart(event, definition.id);
        }
      }}
      onDrop={(event) => onDrop(event, definition.id)}
    >
      <span className="control-panel-activity-icon" aria-hidden="true">
        {definition.icon}
      </span>
      {renderedBadge !== null ? (
        <span className="control-panel-activity-badge" aria-hidden="true">
          {renderedBadge}
        </span>
      ) : null}
    </button>
  );
}

function ControlPanelActionButton({
  definition,
  isExpanded,
  onClick,
}: {
  definition: ControlPanelActionDefinition;
  isExpanded: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className="control-panel-activity-button control-panel-action-button"
      type="button"
      aria-label={definition.label}
      aria-controls="settings-dialog"
      aria-expanded={isExpanded}
      aria-haspopup="dialog"
      title={definition.label}
      onClick={onClick}
    >
      <span className="control-panel-activity-icon" aria-hidden="true">
        {definition.icon}
      </span>
    </button>
  );
}

function resolveDockDropPosition(event: ReactDragEvent<HTMLButtonElement>): DockDropPosition {
  const bounds = event.currentTarget.getBoundingClientRect();
  return event.clientY < bounds.top + bounds.height / 2 ? "before" : "after";
}

function moveSectionOrder(
  currentOrder: readonly ControlPanelSectionId[],
  draggedSectionId: ControlPanelSectionId,
  targetSectionId: ControlPanelSectionId,
  position: DockDropPosition,
): ControlPanelSectionId[] {
  const normalizedOrder = normalizeControlPanelSectionOrder(currentOrder);
  if (draggedSectionId === targetSectionId) {
    return normalizedOrder;
  }

  const withoutDragged = normalizedOrder.filter((sectionId) => sectionId !== draggedSectionId);
  const targetIndex = withoutDragged.indexOf(targetSectionId);
  if (targetIndex === -1) {
    return normalizedOrder;
  }

  const insertIndex = position === "before" ? targetIndex : targetIndex + 1;
  return [
    ...withoutDragged.slice(0, insertIndex),
    draggedSectionId,
    ...withoutDragged.slice(insertIndex),
  ];
}

function normalizeControlPanelSectionOrder(order: readonly ControlPanelSectionId[]): ControlPanelSectionId[] {
  const seen = new Set<ControlPanelSectionId>();
  const normalized: ControlPanelSectionId[] = [];

  for (const sectionId of order) {
    if (!isControlPanelSectionId(sectionId) || seen.has(sectionId)) {
      continue;
    }

    normalized.push(sectionId);
    seen.add(sectionId);
  }

  for (const sectionId of DEFAULT_CONTROL_PANEL_SECTION_ORDER) {
    if (seen.has(sectionId)) {
      continue;
    }

    normalized.push(sectionId);
  }

  return normalized;
}

function isControlPanelSectionId(value: unknown): value is ControlPanelSectionId {
  return DEFAULT_CONTROL_PANEL_SECTION_ORDER.includes(value as ControlPanelSectionId);
}

function getStoredControlPanelSectionOrder(): ControlPanelSectionId[] {
  if (typeof window === "undefined") {
    return [...DEFAULT_CONTROL_PANEL_SECTION_ORDER];
  }

  const stored = parseStoredControlPanelSectionOrder(
    window.localStorage.getItem(CONTROL_PANEL_SECTION_ORDER_STORAGE_KEY),
  );
  if (stored) {
    return normalizeControlPanelSectionOrder(stored);
  }

  return [...DEFAULT_CONTROL_PANEL_SECTION_ORDER];
}

function parseStoredControlPanelSectionOrder(
  rawValue: string | null,
): ControlPanelSectionId[] | null {
  if (!rawValue) {
    return null;
  }

  try {
    const parsed = JSON.parse(rawValue);
    if (!Array.isArray(parsed)) {
      return null;
    }

    return parsed.filter((value): value is ControlPanelSectionId => isControlPanelSectionId(value));
  } catch {
    return null;
  }
}

function persistControlPanelSectionOrder(order: readonly ControlPanelSectionId[]) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(
    CONTROL_PANEL_SECTION_ORDER_STORAGE_KEY,
    JSON.stringify(normalizeControlPanelSectionOrder(order)),
  );
}

function SessionsIcon() {
  return (
    <svg viewBox="0 0 20 20" focusable="false" aria-hidden="true">
      <path
        d="M4.5 5.5h8a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1-1.5 1.5H8l-3 2v-2H4.5A1.5 1.5 0 0 1 3 11V7a1.5 1.5 0 0 1 1.5-1.5Z"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
      <path
        d="M10 8h4.5A1.5 1.5 0 0 1 16 9.5v3A1.5 1.5 0 0 1 14.5 14H14v2l-2.6-2H10"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}

function FilesIcon() {
  return (
    <svg viewBox="0 0 20 20" focusable="false" aria-hidden="true">
      <path
        d="M5 3.5h6l3 3v9A1.5 1.5 0 0 1 12.5 17h-7A1.5 1.5 0 0 1 4 15.5V5A1.5 1.5 0 0 1 5.5 3.5Z"
        fill="none"
        stroke="currentColor"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
      <path d="M11 3.75v3.1h3.1" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path
        d="M6.5 9.25h1.75M6.5 12h5.75M6.5 14.75h5.75"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}

function ProjectsIcon() {
  return (
    <svg viewBox="0 0 20 20" focusable="false" aria-hidden="true">
      <path
        d="M3.5 6.5A1.5 1.5 0 0 1 5 5h3l1.4 1.5H15A1.5 1.5 0 0 1 16.5 8v6A1.5 1.5 0 0 1 15 15.5H5A1.5 1.5 0 0 1 3.5 14V6.5Z"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
      <path d="M3.5 8.5h13" fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

function OrchestratorsIcon() {
  return (
    <svg viewBox="0 0 20 20" focusable="false" aria-hidden="true">
      <circle cx="10" cy="5.5" r="2.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="5" cy="16.5" r="1.8" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="15" cy="16.5" r="1.8" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path
        d="M8.2 7.5 5.8 14.7M11.8 7.5l2.4 7.2"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}

function GitStatusIcon() {
  return <GitHubMark className="control-panel-activity-symbol-github" />;
}

function SettingsIcon() {
  return (
    <svg viewBox="0 0 20 20" focusable="false" aria-hidden="true">
      <path
        d="M5 6.5h10M7.5 10h5M4 13.5h12"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.5"
      />
      <circle cx="7" cy="6.5" r="1.6" fill="var(--panel)" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="12" cy="10" r="1.6" fill="var(--panel)" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="9" cy="13.5" r="1.6" fill="var(--panel)" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}
