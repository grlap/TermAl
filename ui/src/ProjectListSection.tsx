// The "Projects" section rendered in the control panel sidebar:
// an "All projects" aggregate row, one row per project, and a
// right-click context menu with "Start new session" and "Remove
// project" actions.
//
// What this file owns:
//   - The `<section class="project-controls">` layout, the project
//     rows, the project-count badge, and the per-project context
//     menu portal.
//   - The local UI state for the context menu
//     (`contextMenu` + `contextMenuStyle`), the ref used to
//     measure the menu's bounding rect, and the
//     `openContextMenu` / `closeContextMenu` /
//     `updateContextMenuPosition` helpers.
//   - The window-level event handlers that dismiss the context
//     menu (pointerdown outside the menu, Escape, resize,
//     capture-phase scroll).
//   - The `ProjectContextMenu` and `ProjectListSectionProps`
//     types.
//
// What this file does NOT own:
//   - Project list state / filter selection — that lives in
//     `App.tsx`; this component is controlled via its props.
//   - The "Start new session" / "Remove project" side effects —
//     the caller's `onStartSession` / `onRemoveProject` callbacks
//     do the actual work.
//   - The `ALL_PROJECTS_FILTER_ID` sentinel — it lives in
//     `./project-filters` so both this component and `App.tsx`
//     can import it without creating a circular dependency.
//
// Split out of `ui/src/App.tsx`. Same JSX, same classNames, same
// ARIA wiring, same event handlers, same portal target as the
// inline definition it replaced.

import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { createPortal } from "react-dom";

import { clamp } from "./app-utils";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import { describeProjectScope } from "./session-model-utils";
import type { Project, RemoteConfig } from "./types";

export type ProjectContextMenu = {
  clientX: number;
  clientY: number;
  paneId: string | null;
  projectId: string;
};

export type ProjectListSectionProps = {
  paneId: string;
  projectSessionCounts: ReadonlyMap<string, number>;
  projects: Project[];
  remoteLookup: ReadonlyMap<string, RemoteConfig>;
  selectedProjectId: string;
  sessionCount: number;
  onProjectScopeChange: (projectId: string) => void;
  onRemoveProject: (project: Project) => void;
  onStartSession: (paneId: string | null, projectId: string) => void;
};

export function ProjectListSection({
  paneId,
  projectSessionCounts,
  projects,
  remoteLookup,
  selectedProjectId,
  sessionCount,
  onProjectScopeChange,
  onRemoveProject,
  onStartSession,
}: ProjectListSectionProps) {
  const [contextMenu, setContextMenu] = useState<ProjectContextMenu | null>(
    null,
  );
  const [contextMenuStyle, setContextMenuStyle] =
    useState<CSSProperties | null>(null);
  const contextMenuRef = useRef<HTMLDivElement | null>(null);
  const contextMenuProject = contextMenu
    ? (projects.find((project) => project.id === contextMenu.projectId) ?? null)
    : null;

  function closeContextMenu() {
    setContextMenu(null);
    setContextMenuStyle(null);
  }

  function updateContextMenuPosition(
    menu = contextMenu,
    node = contextMenuRef.current,
  ) {
    if (!menu) {
      setContextMenuStyle(null);
      return;
    }

    if (!node || typeof window === "undefined") {
      setContextMenuStyle({
        left: menu.clientX,
        top: menu.clientY,
      });
      return;
    }

    const menuRect = node.getBoundingClientRect();
    const viewportPadding = 12;
    const left = clamp(
      menu.clientX,
      viewportPadding,
      window.innerWidth - menuRect.width - viewportPadding,
    );
    const top = clamp(
      menu.clientY,
      viewportPadding,
      window.innerHeight - menuRect.height - viewportPadding,
    );

    setContextMenuStyle({
      left,
      top,
    });
  }

  function openContextMenu(
    event: ReactMouseEvent<HTMLButtonElement>,
    project: Project,
  ) {
    event.preventDefault();
    event.stopPropagation();
    setContextMenu({
      clientX: event.clientX,
      clientY: event.clientY,
      paneId,
      projectId: project.id,
    });
    setContextMenuStyle({
      left: event.clientX,
      top: event.clientY,
    });
  }

  useEffect(() => {
    if (!contextMenu) {
      return;
    }

    function handlePointerDown(event: PointerEvent) {
      const target = event.target;
      if (target instanceof Node && contextMenuRef.current?.contains(target)) {
        return;
      }

      closeContextMenu();
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        closeContextMenu();
      }
    }

    function handleViewportChange() {
      closeContextMenu();
    }

    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("resize", handleViewportChange);
    window.addEventListener("scroll", handleViewportChange, true);

    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("resize", handleViewportChange);
      window.removeEventListener("scroll", handleViewportChange, true);
    };
  }, [contextMenu]);

  useEffect(() => {
    if (!contextMenu || contextMenuProject) {
      return;
    }

    closeContextMenu();
  }, [contextMenu, contextMenuProject]);

  useLayoutEffect(() => {
    if (!contextMenu) {
      setContextMenuStyle(null);
      return;
    }

    updateContextMenuPosition();
  }, [contextMenu]);

  return (
    <section className="control-panel-section-stack" aria-label="Projects">
      <section className="project-controls" aria-label="Projects">
        <div className="project-controls-header">
          <div className="session-control-label">Projects</div>
          <span className="project-count-badge">{projects.length}</span>
        </div>
        <div className="project-list" role="list">
          <button
            className={`project-row ${selectedProjectId === ALL_PROJECTS_FILTER_ID ? "selected" : ""}`}
            type="button"
            onClick={() => onProjectScopeChange(ALL_PROJECTS_FILTER_ID)}
          >
            <span className="project-row-copy">
              <strong>All projects</strong>
              <span className="project-row-path">
                Show every session in this window.
              </span>
            </span>
            <span className="project-row-count">{sessionCount}</span>
          </button>
          {projects.map((project) => {
            const isSelected = project.id === selectedProjectId;

            return (
              <button
                key={project.id}
                className={`project-row ${isSelected ? "selected" : ""}`}
                type="button"
                onClick={() => onProjectScopeChange(project.id)}
                onContextMenu={(event) => openContextMenu(event, project)}
                aria-haspopup="menu"
                aria-expanded={
                  contextMenu?.projectId === project.id ? "true" : undefined
                }
              >
                <span className="project-row-copy">
                  <strong>{project.name}</strong>
                  <span className="project-row-path">
                    {describeProjectScope(project, remoteLookup)}
                  </span>
                </span>
                <span className="project-row-count">
                  {projectSessionCounts.get(project.id) ?? 0}
                </span>
              </button>
            );
          })}
        </div>
      </section>
      {contextMenu && contextMenuProject && typeof document !== "undefined"
        ? createPortal(
            <div
              ref={contextMenuRef}
              className="context-menu pane-tab-context-menu panel project-context-menu"
              role="menu"
              aria-label={`${contextMenuProject.name} project actions`}
              style={
                contextMenuStyle ?? {
                  left: contextMenu.clientX,
                  top: contextMenu.clientY,
                }
              }
            >
              <button
                className="context-menu-item pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                onClick={() => {
                  closeContextMenu();
                  onStartSession(contextMenu.paneId, contextMenu.projectId);
                }}
              >
                Start new session
              </button>
              <button
                className="context-menu-item context-menu-item-danger pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                onClick={() => {
                  closeContextMenu();
                  onRemoveProject(contextMenuProject);
                }}
              >
                Remove project
              </button>
            </div>,
            document.body,
          )
        : null}
    </section>
  );
}
