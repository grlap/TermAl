import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";

export type ControlPanelSectionId = "sessions" | "projects" | "git";

type ControlPanelSurfaceProps = {
  gitStatusCount: number;
  isPreferencesOpen: boolean;
  onOpenPreferences: () => void;
  projectCount: number;
  renderSection: (sectionId: ControlPanelSectionId) => ReactNode;
  sessionCount: number;
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

const GITHUB_MARK_URL = "https://upload.wikimedia.org/wikipedia/commons/9/91/Octicons-mark-github.svg";

const PREFERENCES_ACTION: ControlPanelActionDefinition = {
  label: "Open preferences",
  icon: <SettingsIcon />,
};

export const ControlPanelSurface = forwardRef<ControlPanelSurfaceHandle, ControlPanelSurfaceProps>(function ControlPanelSurface({
  gitStatusCount,
  isPreferencesOpen,
  onOpenPreferences,
  projectCount,
  renderSection,
  sessionCount,
}, ref): JSX.Element {
  const [activeSection, setActiveSection] = useState<ControlPanelSectionId>("sessions");
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const sectionDefinitions: ReadonlyArray<ControlPanelSectionDefinition> = [
    {
      badgeCount: sessionCount,
      id: "sessions",
      label: "Sessions",
      icon: <SessionsIcon />,
    },
    {
      badgeCount: projectCount,
      id: "projects",
      label: "Projects",
      icon: <ProjectsIcon />,
    },
    {
      badgeCount: gitStatusCount,
      id: "git",
      label: "Git status",
      icon: <GitStatusIcon />,
    },
  ];
  const activeSectionDefinition =
    sectionDefinitions.find((definition) => definition.id === activeSection) ?? sectionDefinitions[0];

  useImperativeHandle(ref, () => ({
    selectSection(sectionId) {
      setActiveSection(sectionId);
    },
  }));

  useEffect(() => {
    if (bodyRef.current) {
      bodyRef.current.scrollTop = 0;
    }
  }, [activeSection]);

  return (
    <div className="control-panel-shell">
      <nav className="control-panel-activity-rail" aria-label="Control panel dock">
        <div className="control-panel-activity-group">
          {sectionDefinitions.map((definition) => (
            <ControlPanelActivityButton
              key={definition.id}
              definition={definition}
              isActive={activeSection === definition.id}
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

      <section className="control-panel-content">
        <header className="control-panel-header">
          <h2>{activeSectionDefinition.label}</h2>
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
  isActive,
  onSelect,
}: {
  definition: ControlPanelSectionDefinition;
  isActive: boolean;
  onSelect: (sectionId: ControlPanelSectionId) => void;
}) {
  const showBadge = Number.isFinite(definition.badgeCount) && (definition.badgeCount ?? 0) > 0;
  const renderedBadge = showBadge ? Math.min(definition.badgeCount ?? 0, 99) : null;

  return (
    <button
      className={`control-panel-activity-button${isActive ? " selected" : ""}`}
      type="button"
      aria-label={definition.label}
      aria-pressed={isActive}
      title={definition.label}
      onClick={() => onSelect(definition.id)}
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

function GitStatusIcon() {
  return (
    <span
      className="control-panel-activity-symbol control-panel-activity-symbol-github"
      style={{ "--control-panel-icon-mask": `url(${GITHUB_MARK_URL})` } as CSSProperties}
    />
  );
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
