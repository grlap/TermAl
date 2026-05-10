import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { createPortal } from "react-dom";
import {
  areRemoteConfigsEqual,
  CLAUDE_EFFORT_OPTIONS,
  CODEX_REASONING_EFFORT_OPTIONS,
  DEFAULT_MODEL_PREFERENCE,
  isDefaultModelPreference,
  MAX_DEFAULT_MODEL_PREFERENCE_CHARS,
  remoteBadgeLabel,
  type ComboboxOption,
} from "./session-model-utils";
import {
  fetchTelegramStatus,
  testTelegramConnection,
  updateTelegramConfig,
  type TelegramStatusResponse,
} from "./api";
import {
  DENSITY_STEP_PERCENT,
  DEFAULT_DENSITY_PERCENT,
  DEFAULT_EDITOR_FONT_SIZE_PX,
  DEFAULT_FONT_SIZE_PX,
  DIAGRAM_LOOKS,
  DIAGRAM_PALETTES,
  MARKDOWN_STYLES,
  MARKDOWN_THEMES,
  MAX_DENSITY_PERCENT,
  MAX_EDITOR_FONT_SIZE_PX,
  MAX_FONT_SIZE_PX,
  MIN_DENSITY_PERCENT,
  MIN_EDITOR_FONT_SIZE_PX,
  MIN_FONT_SIZE_PX,
  STYLES,
  THEMES,
  type DiagramLook,
  type DiagramPalette,
  type DiagramThemeOverrideMode,
  type MarkdownStyleId,
  type MarkdownThemeId,
  type StyleId,
  type ThemeId,
} from "./themes";
import { clamp } from "./app-utils";
import {
  isLocalRemoteId,
  remoteConnectionLabel,
} from "./remotes";
import type {
  AgentType,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CursorMode,
  GeminiApprovalMode,
  Project,
  RemoteConfig,
  SandboxMode,
  Session,
} from "./types";

export const SANDBOX_MODE_OPTIONS = [
  { label: "workspace-write", value: "workspace-write" },
  { label: "read-only", value: "read-only" },
  { label: "danger-full-access", value: "danger-full-access" },
] as const;
export const APPROVAL_POLICY_OPTIONS = [
  { label: "never", value: "never" },
  { label: "on-request", value: "on-request" },
  { label: "untrusted", value: "untrusted" },
  { label: "on-failure", value: "on-failure" },
] as const;
export const CLAUDE_APPROVAL_OPTIONS = [
  { label: "ask", value: "ask" },
  { label: "auto-approve", value: "auto-approve" },
  { label: "plan", value: "plan" },
] as const;
export const CURSOR_MODE_OPTIONS = [
  {
    label: "agent",
    value: "agent",
    description: "Allow edits and auto-approve tool requests",
  },
  {
    label: "plan",
    value: "plan",
    description: "Stay read-only and deny tool requests",
  },
  {
    label: "ask",
    value: "ask",
    description: "Keep approval cards before tool use",
  },
] as const;
export const GEMINI_APPROVAL_OPTIONS = [
  { label: "default", value: "default" },
  { label: "auto_edit", value: "auto_edit" },
  { label: "yolo", value: "yolo" },
  { label: "plan", value: "plan" },
] as const;

function normalizeDefaultModelPreferenceDraft(value: string) {
  return isDefaultModelPreference(value)
    ? DEFAULT_MODEL_PREFERENCE
    : value.trim();
}

function displayDefaultModelPreference(value: string) {
  return isDefaultModelPreference(value)
    ? DEFAULT_MODEL_PREFERENCE
    : value;
}

function clampDefaultModelPreferenceDraft(value: string) {
  const characters = Array.from(value);
  if (characters.length <= MAX_DEFAULT_MODEL_PREFERENCE_CHARS) {
    return value;
  }

  return characters.slice(0, MAX_DEFAULT_MODEL_PREFERENCE_CHARS).join("");
}

function AgentDefaultModelControl({
  agent,
  id,
  value,
  onChange,
}: {
  agent: AgentType;
  id: string;
  value: string;
  onChange: (nextValue: string) => void;
}) {
  const [draft, setDraft] = useState(displayDefaultModelPreference(value));
  const [isDirty, setIsDirty] = useState(false);
  const [pendingAppliedValue, setPendingAppliedValue] = useState<string | null>(null);

  useEffect(() => {
    setIsDirty(false);
    setPendingAppliedValue(null);
    setDraft(displayDefaultModelPreference(value));
    // Reset only when switching controls; value echoes are handled below without clobbering dirty drafts.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  const normalizedDraft = normalizeDefaultModelPreferenceDraft(draft);
  const normalizedValue = normalizeDefaultModelPreferenceDraft(value);
  const canApply = normalizedDraft !== normalizedValue;
  const hintId = `${id}-hint`;

  useEffect(() => {
    if (pendingAppliedValue !== null) {
      if (normalizedValue === pendingAppliedValue) {
        setDraft(displayDefaultModelPreference(value));
        setIsDirty(false);
        setPendingAppliedValue(null);
      }
      return;
    }

    if (!isDirty) {
      setDraft(displayDefaultModelPreference(value));
    }
  }, [isDirty, normalizedValue, pendingAppliedValue, value]);

  function updateDraft(nextDraft: string) {
    const clampedDraft = clampDefaultModelPreferenceDraft(nextDraft);
    setDraft(clampedDraft);
    setIsDirty(
      normalizeDefaultModelPreferenceDraft(clampedDraft) !== normalizedValue,
    );
    setPendingAppliedValue(null);
  }

  function applyDraft() {
    if (!canApply) {
      return;
    }

    setDraft(normalizedDraft);
    setIsDirty(true);
    setPendingAppliedValue(normalizedDraft);
    onChange(normalizedDraft);
  }

  return (
    <div className="session-control-group">
      <label className="session-control-label" htmlFor={id}>
        Default model
      </label>
      <div className="session-model-custom-row">
        <input
          id={id}
          className="themed-input session-model-custom-input"
          type="text"
          value={draft}
          placeholder="default"
          aria-describedby={hintId}
          aria-label={`${agent} default model`}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          onChange={(event) => updateDraft(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter") {
              return;
            }
            if (!canApply) {
              return;
            }

            event.preventDefault();
            applyDraft();
          }}
        />
        <button
          type="button"
          className="ghost-button session-model-custom-apply"
          aria-describedby={hintId}
          aria-label={`Apply ${agent} default model`}
          disabled={!canApply}
          onClick={applyDraft}
        >
          Apply
        </button>
        <button
          type="button"
          className="ghost-button session-model-custom-reset"
          aria-describedby={hintId}
          aria-label={`Reset ${agent} default model`}
          disabled={isDefaultModelPreference(value)}
          onClick={() => {
            setDraft(DEFAULT_MODEL_PREFERENCE);
            setIsDirty(true);
            setPendingAppliedValue(DEFAULT_MODEL_PREFERENCE);
            onChange(DEFAULT_MODEL_PREFERENCE);
          }}
        >
          Reset
        </button>
      </div>
      <p id={hintId} className="session-control-hint">
        Use <code>default</code> or leave blank to let {agent} choose its built-in default, or enter an exact model id.
      </p>
    </div>
  );
}

export function ThemePreferencesPanel({
  activeStyle,
  activeTheme,
  styleId,
  themeId,
  onSelectStyle,
  onSelectTheme,
}: {
  activeStyle: (typeof STYLES)[number];
  activeTheme: (typeof THEMES)[number];
  styleId: StyleId;
  themeId: ThemeId;
  onSelectStyle: (styleId: StyleId) => void;
  onSelectTheme: (themeId: ThemeId) => void;
}) {
  return (
    <section className="settings-panel-stack theme-preferences-layout">
      <StylePicker
        activeStyle={activeStyle}
        styleId={styleId}
        onSelectStyle={onSelectStyle}
        compact
      />
      <ThemePicker activeTheme={activeTheme} themeId={themeId} onSelectTheme={onSelectTheme} />
    </section>
  );
}

export function ThemePicker({
  activeTheme,
  themeId,
  onSelectTheme,
}: {
  activeTheme: (typeof THEMES)[number];
  themeId: ThemeId;
  onSelectTheme: (themeId: ThemeId) => void;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const dragStateRef = useRef<{
    pointerId: number;
    startPointerY: number;
    startThumbOffset: number;
  } | null>(null);
  const [scrollState, setScrollState] = useState({
    thumbHeight: 0,
    thumbOffset: 0,
    visible: false,
  });
  const [isDraggingScrollbar, setIsDraggingScrollbar] = useState(false);

  useLayoutEffect(() => {
    const listElement = listRef.current;
    if (!listElement) {
      return;
    }

    const updateScrollState = () => {
      const { clientHeight, scrollHeight, scrollTop } = listElement;
      const hasOverflow = scrollHeight - clientHeight > 1;

      if (!hasOverflow) {
        setScrollState((current) =>
          current.visible || current.thumbHeight !== 0 || current.thumbOffset !== 0
            ? { thumbHeight: 0, thumbOffset: 0, visible: false }
            : current,
        );
        return;
      }

      const thumbHeight = Math.max((clientHeight * clientHeight) / scrollHeight, 48);
      const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
      const maxScrollTop = Math.max(scrollHeight - clientHeight, 1);
      const thumbOffset = (scrollTop / maxScrollTop) * maxThumbOffset;

      setScrollState((current) => {
        if (
          current.visible &&
          Math.abs(current.thumbHeight - thumbHeight) < 0.5 &&
          Math.abs(current.thumbOffset - thumbOffset) < 0.5
        ) {
          return current;
        }

        return {
          thumbHeight,
          thumbOffset,
          visible: true,
        };
      });
    };

    updateScrollState();

    listElement.addEventListener("scroll", updateScrollState);
    window.addEventListener("resize", updateScrollState);

    const resizeObserver =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(() => {
            updateScrollState();
          });

    resizeObserver?.observe(listElement);

    return () => {
      listElement.removeEventListener("scroll", updateScrollState);
      window.removeEventListener("resize", updateScrollState);
      resizeObserver?.disconnect();
    };
  }, []);

  useEffect(() => {
    function handlePointerMove(event: PointerEvent) {
      const dragState = dragStateRef.current;
      const listElement = listRef.current;
      if (!dragState || !listElement || event.pointerId !== dragState.pointerId) {
        return;
      }

      const { clientHeight, scrollHeight } = listElement;
      const thumbHeight = Math.max((clientHeight * clientHeight) / Math.max(scrollHeight, 1), 48);
      const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
      const nextThumbOffset = clamp(
        dragState.startThumbOffset + (event.clientY - dragState.startPointerY),
        0,
        maxThumbOffset,
      );
      const maxScrollTop = Math.max(scrollHeight - clientHeight, 0);

      listElement.scrollTop =
        maxThumbOffset > 0 ? (nextThumbOffset / maxThumbOffset) * maxScrollTop : 0;
    }

    function handlePointerUp(event: PointerEvent) {
      const dragState = dragStateRef.current;
      if (!dragState || event.pointerId !== dragState.pointerId) {
        return;
      }

      dragStateRef.current = null;
      setIsDraggingScrollbar(false);
    }

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("pointercancel", handlePointerUp);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("pointercancel", handlePointerUp);
    };
  }, []);

  function scrollListToThumbOffset(nextThumbOffset: number) {
    const listElement = listRef.current;
    if (!listElement) {
      return;
    }

    const { clientHeight, scrollHeight } = listElement;
    const thumbHeight = Math.max((clientHeight * clientHeight) / Math.max(scrollHeight, 1), 48);
    const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
    const clampedThumbOffset = clamp(nextThumbOffset, 0, maxThumbOffset);
    const maxScrollTop = Math.max(scrollHeight - clientHeight, 0);

    listElement.scrollTop =
      maxThumbOffset > 0 ? (clampedThumbOffset / maxThumbOffset) * maxScrollTop : 0;
  }

  function handleScrollbarThumbPointerDown(event: ReactPointerEvent<HTMLSpanElement>) {
    event.preventDefault();
    event.stopPropagation();

    dragStateRef.current = {
      pointerId: event.pointerId,
      startPointerY: event.clientY,
      startThumbOffset: scrollState.thumbOffset,
    };
    setIsDraggingScrollbar(true);
  }

  function handleScrollbarTrackPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();

    const trackRect = event.currentTarget.getBoundingClientRect();
    const nextThumbOffset = event.clientY - trackRect.top - scrollState.thumbHeight / 2;
    const { clientHeight } = event.currentTarget;
    const clampedThumbOffset = clamp(
      nextThumbOffset,
      0,
      Math.max(clientHeight - scrollState.thumbHeight, 0),
    );

    scrollListToThumbOffset(clampedThumbOffset);
    dragStateRef.current = {
      pointerId: event.pointerId,
      startPointerY: event.clientY,
      startThumbOffset: clampedThumbOffset,
    };
    setIsDraggingScrollbar(true);
  }

  return (
    <section className="theme-panel">
      <div className="theme-panel-header">
        <div>
          <p className="session-control-label">Theme</p>
          <p className="theme-panel-copy">{activeTheme.description}</p>
        </div>
      </div>

      <div className="theme-option-list-shell">
        <div ref={listRef} className="theme-option-list" role="radiogroup" aria-label="UI theme">
          {THEMES.map((theme) => {
            const isSelected = theme.id === themeId;

            return (
              <button
                key={theme.id}
                className={`theme-option ${isSelected ? "selected" : ""}`}
                type="button"
                role="radio"
                aria-checked={isSelected}
                onClick={() => onSelectTheme(theme.id)}
              >
                <span className="theme-option-main">
                  <span className="theme-option-title-row">
                    <strong className="theme-option-title">{theme.name}</strong>
                  </span>
                  <span className="theme-option-copy">{theme.description}</span>
                </span>
                <span className="theme-option-preview" aria-hidden="true">
                  {theme.swatches.map((swatch) => (
                    <span
                      key={`${theme.id}-${swatch}`}
                      className="theme-option-swatch"
                      style={{ background: swatch }}
                    />
                  ))}
                </span>
              </button>
            );
          })}
        </div>
        {scrollState.visible ? (
          <div
            className={`theme-option-scrollbar ${isDraggingScrollbar ? "dragging" : ""}`}
            aria-hidden="true"
            onPointerDown={handleScrollbarTrackPointerDown}
          >
            <span
              className="theme-option-scrollbar-thumb"
              onPointerDown={handleScrollbarThumbPointerDown}
              style={{
                height: `${scrollState.thumbHeight}px`,
                transform: `translateY(${scrollState.thumbOffset}px)`,
              }}
            />
          </div>
        ) : null}
      </div>
    </section>
  );
}

export function StyleTreatmentPreview({ styleId }: { styleId: StyleId }) {
  return (
    <span
      className={`theme-option-preview style-treatment-preview style-treatment-preview-${styleId}`.trim()}
      aria-hidden="true"
    >
      <span className="style-treatment-preview-frame">
        <span className="style-treatment-preview-header">
          <span className="style-treatment-preview-dot" />
          <span className="style-treatment-preview-line" />
        </span>
        <span className="style-treatment-preview-block style-treatment-preview-block-primary" />
        <span className="style-treatment-preview-block style-treatment-preview-block-secondary" />
      </span>
    </span>
  );
}

export function StylePicker({
  activeStyle,
  compact = false,
  styleId,
  onSelectStyle,
}: {
  activeStyle: (typeof STYLES)[number];
  compact?: boolean;
  styleId: StyleId;
  onSelectStyle: (styleId: StyleId) => void;
}) {
  return (
    <section className={`theme-panel style-panel ${compact ? "style-panel-compact" : ""}`.trim()}>
      <div className="theme-panel-header">
        <div>
          <p className="session-control-label">Style</p>
          <p className="theme-panel-copy">{activeStyle.description}</p>
        </div>
      </div>

      <div className="theme-option-list-shell">
        <div
          className={`theme-option-list style-option-list ${compact ? "style-option-list-compact" : ""}`.trim()}
          role="radiogroup"
          aria-label="UI style"
        >
          {STYLES.map((style) => {
            const isSelected = style.id === styleId;

            return (
              <button
                key={style.id}
                className={`theme-option ${isSelected ? "selected" : ""}`}
                type="button"
                role="radio"
                aria-checked={isSelected}
                title={style.description}
                onClick={() => onSelectStyle(style.id)}
              >
                <StyleTreatmentPreview styleId={style.id} />
                <span className="theme-option-main">
                  <span className="theme-option-title-row">
                    <strong className="theme-option-title">{style.name}</strong>
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </section>
  );
}

export function MarkdownPreferencesPanel({
  activeMarkdownTheme,
  activeMarkdownStyle,
  markdownThemeId,
  markdownStyleId,
  diagramThemeOverrideMode,
  diagramLook,
  diagramPalette,
  onSelectMarkdownTheme,
  onSelectMarkdownStyle,
  onSelectDiagramThemeOverrideMode,
  onSelectDiagramLook,
  onSelectDiagramPalette,
}: {
  activeMarkdownTheme: (typeof MARKDOWN_THEMES)[number];
  activeMarkdownStyle: (typeof MARKDOWN_STYLES)[number];
  markdownThemeId: MarkdownThemeId;
  markdownStyleId: MarkdownStyleId;
  diagramThemeOverrideMode: DiagramThemeOverrideMode;
  diagramLook: DiagramLook;
  diagramPalette: DiagramPalette;
  onSelectMarkdownTheme: (id: MarkdownThemeId) => void;
  onSelectMarkdownStyle: (id: MarkdownStyleId) => void;
  onSelectDiagramThemeOverrideMode: (mode: DiagramThemeOverrideMode) => void;
  onSelectDiagramLook: (look: DiagramLook) => void;
  onSelectDiagramPalette: (palette: DiagramPalette) => void;
}) {
  const activeDiagramLook =
    DIAGRAM_LOOKS.find((option) => option.id === diagramLook) ??
    DIAGRAM_LOOKS[0];
  const activeDiagramPalette =
    DIAGRAM_PALETTES.find((option) => option.id === diagramPalette) ??
    DIAGRAM_PALETTES[0];
  const diagramOverrideOptions: ReadonlyArray<{
    id: DiagramThemeOverrideMode;
    name: string;
    description: string;
  }> = [
    {
      id: "on",
      name: "Override",
      description:
        "Strip author themes from Mermaid diagrams so the Markdown theme always wins. Coherent across a mixed document.",
    },
    {
      id: "off",
      name: "Respect",
      description:
        "Honor diagram-author `%%{init: ...}%%` directives. Useful when preserving original styling matters.",
    },
  ];
  return (
    <section className="settings-panel-stack theme-preferences-layout">
      <section className="theme-panel style-panel style-panel-compact">
        <div className="theme-panel-header">
          <div>
            <p className="session-control-label">Markdown style</p>
            <p className="theme-panel-copy">{activeMarkdownStyle.description}</p>
          </div>
        </div>

        <div className="theme-option-list-shell">
          <div
            className="theme-option-list style-option-list style-option-list-compact"
            role="radiogroup"
            aria-label="Markdown style"
          >
            {MARKDOWN_STYLES.map((style) => {
              const isSelected = style.id === markdownStyleId;
              return (
                <button
                  key={style.id}
                  className={`theme-option ${isSelected ? "selected" : ""}`}
                  type="button"
                  role="radio"
                  aria-checked={isSelected}
                  title={style.description}
                  onClick={() => onSelectMarkdownStyle(style.id)}
                >
                  <span className="theme-option-main">
                    <span className="theme-option-title-row">
                      <strong className="theme-option-title">{style.name}</strong>
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      </section>

      <section className="theme-panel">
        <div className="theme-panel-header">
          <div>
            <p className="session-control-label">Markdown theme</p>
            <p className="theme-panel-copy">{activeMarkdownTheme.description}</p>
          </div>
        </div>

        <div className="theme-option-list-shell">
          <div
            className="theme-option-list"
            role="radiogroup"
            aria-label="Markdown theme"
          >
            {MARKDOWN_THEMES.map((theme) => {
              const isSelected = theme.id === markdownThemeId;
              return (
                <button
                  key={theme.id}
                  className={`theme-option ${isSelected ? "selected" : ""}`}
                  type="button"
                  role="radio"
                  aria-checked={isSelected}
                  onClick={() => onSelectMarkdownTheme(theme.id)}
                >
                  <span className="theme-option-main">
                    <span className="theme-option-title-row">
                      <strong className="theme-option-title">{theme.name}</strong>
                    </span>
                    <span className="theme-option-copy">{theme.description}</span>
                  </span>
                  <span className="theme-option-preview" aria-hidden="true">
                    {theme.swatches.map((swatch, swatchIndex) => (
                      <span
                        key={`${theme.id}-${swatchIndex}-${swatch}`}
                        className="theme-option-swatch"
                        style={
                          swatch === "inherit"
                            ? { background: "transparent", border: "1px dashed currentColor" }
                            : { background: swatch }
                        }
                      />
                    ))}
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      </section>

      <section className="theme-panel style-panel style-panel-compact">
        <div className="theme-panel-header">
          <div>
            <p className="session-control-label">Diagram theme override</p>
            <p className="theme-panel-copy">
              {
                (
                  diagramOverrideOptions.find(
                    (option) => option.id === diagramThemeOverrideMode,
                  ) ?? diagramOverrideOptions[0]
                ).description
              }
            </p>
          </div>
        </div>

        <div className="theme-option-list-shell">
          <div
            className="theme-option-list style-option-list style-option-list-compact"
            role="radiogroup"
            aria-label="Diagram theme override"
          >
            {diagramOverrideOptions.map((option) => {
              const isSelected = option.id === diagramThemeOverrideMode;
              return (
                <button
                  key={option.id}
                  className={`theme-option ${isSelected ? "selected" : ""}`}
                  type="button"
                  role="radio"
                  aria-checked={isSelected}
                  title={option.description}
                  onClick={() => onSelectDiagramThemeOverrideMode(option.id)}
                >
                  <span className="theme-option-main">
                    <span className="theme-option-title-row">
                      <strong className="theme-option-title">{option.name}</strong>
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      </section>

      <section className="theme-panel style-panel style-panel-compact">
        <div className="theme-panel-header">
          <div>
            <p className="session-control-label">Diagram look</p>
            <p className="theme-panel-copy">{activeDiagramLook.description}</p>
          </div>
        </div>

        <div className="theme-option-list-shell">
          <div
            className="theme-option-list style-option-list style-option-list-compact"
            role="radiogroup"
            aria-label="Diagram look"
          >
            {DIAGRAM_LOOKS.map((option) => {
              const isSelected = option.id === diagramLook;
              return (
                <button
                  key={option.id}
                  className={`theme-option ${isSelected ? "selected" : ""}`}
                  type="button"
                  role="radio"
                  aria-checked={isSelected}
                  title={option.description}
                  onClick={() => onSelectDiagramLook(option.id)}
                >
                  <span className="theme-option-main">
                    <span className="theme-option-title-row">
                      <strong className="theme-option-title">{option.name}</strong>
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      </section>

      <section className="theme-panel style-panel style-panel-compact">
        <div className="theme-panel-header">
          <div>
            <p className="session-control-label">Diagram palette</p>
            <p className="theme-panel-copy">{activeDiagramPalette.description}</p>
          </div>
        </div>

        <div className="theme-option-list-shell">
          <div
            className="theme-option-list style-option-list style-option-list-compact"
            role="radiogroup"
            aria-label="Diagram palette"
          >
            {DIAGRAM_PALETTES.map((option) => {
              const isSelected = option.id === diagramPalette;
              return (
                <button
                  key={option.id}
                  className={`theme-option ${isSelected ? "selected" : ""}`}
                  type="button"
                  role="radio"
                  aria-checked={isSelected}
                  title={option.description}
                  onClick={() => onSelectDiagramPalette(option.id)}
                >
                  <span className="theme-option-main">
                    <span className="theme-option-title-row">
                      <strong className="theme-option-title">{option.name}</strong>
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      </section>
    </section>
  );
}

export function AppearancePreferencesPanel({
  densityPercent,
  editorFontSizePx,
  fontSizePx,
  onSelectDensity,
  onSelectEditorFontSize,
  onSelectFontSize,
}: {
  densityPercent: number;
  editorFontSizePx: number;
  fontSizePx: number;
  onSelectDensity: (densityPercent: number) => void;
  onSelectEditorFontSize: (fontSizePx: number) => void;
  onSelectFontSize: (fontSizePx: number) => void;
}) {
  const canDecreaseFontSize = fontSizePx > MIN_FONT_SIZE_PX;
  const canIncreaseFontSize = fontSizePx < MAX_FONT_SIZE_PX;
  const canDecreaseEditorFontSize = editorFontSizePx > MIN_EDITOR_FONT_SIZE_PX;
  const canIncreaseEditorFontSize = editorFontSizePx < MAX_EDITOR_FONT_SIZE_PX;

  return (
    <section className="settings-panel-stack">
      <article className="message-card prompt-settings-card appearance-settings-card">
        <div className="card-label">Appearance</div>
        <h3>Font sizes</h3>
        <div className="prompt-settings-grid appearance-settings-grid">
          <FontSizePreferenceControl
            canDecrease={canDecreaseFontSize}
            canIncrease={canIncreaseFontSize}
            controlsLabel="UI font size controls"
            defaultValue={DEFAULT_FONT_SIZE_PX}
            decreaseId="font-size-decrease"
            label="UI"
            onSelectFontSize={onSelectFontSize}
            resetId="font-size-reset"
            value={fontSizePx}
          />
          <FontSizePreferenceControl
            canDecrease={canDecreaseEditorFontSize}
            canIncrease={canIncreaseEditorFontSize}
            controlsLabel="Editor font size controls"
            defaultValue={DEFAULT_EDITOR_FONT_SIZE_PX}
            decreaseId="editor-font-size-decrease"
            label="Editor"
            onSelectFontSize={onSelectEditorFontSize}
            resetId="editor-font-size-reset"
            value={editorFontSizePx}
          />
          <p className="session-control-hint">
            UI changes apply across the interface. Editor changes affect source and diff editors. Both are saved in this browser.
          </p>
        </div>
      </article>

      <article className="message-card prompt-settings-card appearance-settings-card density-settings-card">
        <div className="card-label">Layout</div>
        <h3>Density</h3>
        <DensityPreferenceControl densityPercent={densityPercent} onSelectDensity={onSelectDensity} />
      </article>
    </section>
  );
}

export function FontSizePreferenceControl({
  canDecrease,
  canIncrease,
  controlsLabel,
  defaultValue,
  decreaseId,
  label,
  onSelectFontSize,
  resetId,
  value,
}: {
  canDecrease: boolean;
  canIncrease: boolean;
  controlsLabel: string;
  defaultValue: number;
  decreaseId: string;
  label: string;
  onSelectFontSize: (fontSizePx: number) => void;
  resetId: string;
  value: number;
}) {
  return (
    <>
      <div className="session-control-group">
        <label className="session-control-label" htmlFor={decreaseId}>
          {label}
        </label>
        <div className="font-size-controls" role="group" aria-label={controlsLabel}>
          <button
            id={decreaseId}
            className="ghost-button font-size-stepper"
            type="button"
            onClick={() => onSelectFontSize(value - 1)}
            disabled={!canDecrease}
          >
            A-
          </button>
          <div className="font-size-readout" aria-live="polite">
            <strong className="font-size-readout-value">{value}px</strong>
            <span className="font-size-readout-copy">
              {value === defaultValue ? "Default" : "Live"}
            </span>
          </div>
          <button
            className="ghost-button font-size-stepper"
            type="button"
            onClick={() => onSelectFontSize(value + 1)}
            disabled={!canIncrease}
          >
            A+
          </button>
        </div>
      </div>
      <div className="session-control-group">
        <label className="session-control-label" htmlFor={resetId}>
          Reset
        </label>
        <button
          id={resetId}
          className="ghost-button font-size-reset"
          type="button"
          onClick={() => onSelectFontSize(defaultValue)}
          disabled={value === defaultValue}
        >
          Use default
        </button>
      </div>
    </>
  );
}

export function DensityPreferenceControl({
  densityPercent,
  onSelectDensity,
}: {
  densityPercent: number;
  onSelectDensity: (densityPercent: number) => void;
}) {
  const densityDescription = describeDensityPreference(densityPercent);

  return (
    <div className="prompt-settings-grid appearance-settings-grid density-settings-grid">
      <div className="session-control-group density-range-group">
        <label className="session-control-label" htmlFor="density-scale-slider">
          UI density
        </label>
        <div className="density-slider-shell">
          <input
            id="density-scale-slider"
            className="density-slider"
            type="range"
            min={MIN_DENSITY_PERCENT}
            max={MAX_DENSITY_PERCENT}
            step={DENSITY_STEP_PERCENT}
            value={densityPercent}
            aria-valuetext={`${densityPercent}% ${densityDescription}`}
            onChange={(event) => onSelectDensity(Number.parseInt(event.target.value, 10))}
          />
          <div className="density-slider-labels" aria-hidden="true">
            <span>Compact</span>
            <span>Default</span>
            <span>Comfortable</span>
          </div>
        </div>
      </div>
      <div className="session-control-group">
        <p className="session-control-label">Current</p>
        <div className="font-size-readout density-readout" aria-live="polite">
          <strong className="font-size-readout-value">{densityPercent}%</strong>
          <span className="font-size-readout-copy">{densityDescription}</span>
        </div>
      </div>
      <div className="session-control-group">
        <label className="session-control-label" htmlFor="density-reset">
          Reset
        </label>
        <button
          id="density-reset"
          className="ghost-button font-size-reset"
          type="button"
          onClick={() => onSelectDensity(DEFAULT_DENSITY_PERCENT)}
          disabled={densityPercent === DEFAULT_DENSITY_PERCENT}
        >
          Use default
        </button>
      </div>
      <p className="session-control-hint">
        Density scales spacing, control sizes, and pane widths without changing your font settings. It is saved in this browser.
      </p>
    </div>
  );
}

export function describeDensityPreference(densityPercent: number) {
  if (densityPercent < DEFAULT_DENSITY_PERCENT) {
    return densityPercent <= 85 ? "Compact" : "Tight";
  }

  if (densityPercent > DEFAULT_DENSITY_PERCENT) {
    return densityPercent >= 115 ? "Airy" : "Comfortable";
  }

  return "Default";
}

export function validateRemoteDrafts(remotes: RemoteConfig[]): string | null {
  let hasLocalRemote = false;
  const seenIds = new Set<string>();

  for (const remote of remotes) {
    const id = remote.id.trim();
    const name = remote.name.trim();
    if (!id) {
      return "Every remote needs an id.";
    }

    const normalizedId = id.toLowerCase();
    if (seenIds.has(normalizedId)) {
      return `Remote ids must be unique. Duplicate id: ${id}.`;
    }
    seenIds.add(normalizedId);

    if (!name) {
      return `Remote ${id} needs a name.`;
    }

    if (isLocalRemoteId(id)) {
      hasLocalRemote = true;
      continue;
    }

    if (!(remote.host?.trim())) {
      return `Remote ${name} needs a host.`;
    }
    if (remote.port !== null && remote.port !== undefined) {
      if (!Number.isInteger(remote.port) || remote.port <= 0 || remote.port > 65535) {
        return `Remote ${name} has an invalid SSH port.`;
      }
    }
  }

  return hasLocalRemote ? null : "The built-in local remote is required.";
}

export function createSshRemoteDraft(remotes: RemoteConfig[]): RemoteConfig {
  let suffix = remotes.length;
  let id = `ssh-${suffix}`;
  const existingIds = new Set(remotes.map((remote) => remote.id.trim().toLowerCase()));
  while (existingIds.has(id)) {
    suffix += 1;
    id = `ssh-${suffix}`;
  }

  return {
    id,
    name: `SSH Remote ${suffix}`,
    transport: "ssh",
    enabled: true,
    host: "",
    port: 22,
    user: "",
  };
}

export function RemotePreferencesPanel({
  remotes,
  onSaveRemotes,
}: {
  remotes: RemoteConfig[];
  onSaveRemotes: (remotes: RemoteConfig[]) => void;
}) {
  const [draftRemotes, setDraftRemotes] = useState<RemoteConfig[]>(remotes);

  useEffect(() => {
    setDraftRemotes((current) => (areRemoteConfigsEqual(current, remotes) ? current : remotes));
  }, [remotes]);

  const validationError = useMemo(() => validateRemoteDrafts(draftRemotes), [draftRemotes]);
  const hasChanges = useMemo(
    () => JSON.stringify(draftRemotes) !== JSON.stringify(remotes),
    [draftRemotes, remotes],
  );

  function updateRemote(index: number, patch: Partial<RemoteConfig>) {
    setDraftRemotes((current) =>
      current.map((remote, remoteIndex) =>
        remoteIndex === index ? { ...remote, ...patch } : remote,
      ),
    );
  }

  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">Local control plane</p>
          <p className="settings-panel-copy">
            Remote definitions live in the local TermAl server. Projects point at one remote, and SSH-backed execution will route through that mapping.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card remote-settings-card">
        <div className="card-label">Project Routing</div>
        <h3>Remote definitions</h3>
        <div className="remote-settings-list">
          {draftRemotes.map((remote, index) => {
            const isLocal = isLocalRemoteId(remote.id);
            return (
              <article key={`${remote.id}-${index}`} className="remote-settings-row">
                <div className="remote-settings-row-header">
                  <div>
                    <p className="session-control-label">
                      {isLocal ? "Local remote" : remote.name.trim() || `Remote ${index}`}
                    </p>
                    <p className="settings-panel-copy">{remoteConnectionLabel(remote)}</p>
                  </div>
                  <div className="remote-settings-badges">
                    <span className="remote-settings-badge">{remoteBadgeLabel(remote)}</span>
                    <span className="remote-settings-badge">
                      {remote.enabled || isLocal ? "Enabled" : "Disabled"}
                    </span>
                  </div>
                </div>

                <div className="remote-settings-row-fields">
                  <div className="session-control-group">
                    <label className="session-control-label" htmlFor={`remote-id-${index}`}>
                      Remote id
                    </label>
                    <input
                      id={`remote-id-${index}`}
                      className="themed-input"
                      type="text"
                      value={remote.id}
                      onChange={(event) => updateRemote(index, { id: event.target.value })}
                      disabled={isLocal}
                    />
                  </div>
                  <div className="session-control-group">
                    <label className="session-control-label" htmlFor={`remote-name-${index}`}>
                      Name
                    </label>
                    <input
                      id={`remote-name-${index}`}
                      className="themed-input"
                      type="text"
                      value={remote.name}
                      onChange={(event) => updateRemote(index, { name: event.target.value })}
                      disabled={isLocal}
                    />
                  </div>
                  {!isLocal ? (
                    <>
                      <div className="session-control-group">
                        <label className="session-control-label" htmlFor={`remote-host-${index}`}>
                          Host
                        </label>
                        <input
                          id={`remote-host-${index}`}
                          className="themed-input"
                          type="text"
                          value={remote.host ?? ""}
                          onChange={(event) => updateRemote(index, { host: event.target.value })}
                        />
                      </div>
                      <div className="session-control-group">
                        <label className="session-control-label" htmlFor={`remote-user-${index}`}>
                          User
                        </label>
                        <input
                          id={`remote-user-${index}`}
                          className="themed-input"
                          type="text"
                          value={remote.user ?? ""}
                          onChange={(event) => updateRemote(index, { user: event.target.value })}
                        />
                      </div>
                      <div className="session-control-group">
                        <label className="session-control-label" htmlFor={`remote-port-${index}`}>
                          Port
                        </label>
                        <input
                          id={`remote-port-${index}`}
                          className="themed-input"
                          type="number"
                          min={1}
                          max={65535}
                          value={remote.port ?? 22}
                          onChange={(event) => {
                            const value = event.target.value.trim();
                            updateRemote(index, {
                              port: value ? Number.parseInt(value, 10) : null,
                            });
                          }}
                        />
                      </div>
                      <label className="remote-settings-toggle">
                        <input
                          type="checkbox"
                          checked={remote.enabled}
                          onChange={(event) => updateRemote(index, { enabled: event.target.checked })}
                        />
                        <span>Enabled for projects and sessions</span>
                      </label>
                    </>
                  ) : null}
                </div>

                {!isLocal ? (
                  <div className="remote-settings-row-actions">
                    <button
                      className="ghost-button"
                      type="button"
                      onClick={() => {
                        setDraftRemotes((current) => current.filter((_, remoteIndex) => remoteIndex !== index));
                      }}
                    >
                      Remove
                    </button>
                  </div>
                ) : null}
              </article>
            );
          })}
        </div>

        <div className="remote-settings-actions">
          <button
            className="ghost-button"
            type="button"
            onClick={() => {
              setDraftRemotes((current) => [...current, createSshRemoteDraft(current)]);
            }}
          >
            Add SSH remote
          </button>
          <button
            className="ghost-button"
            type="button"
            onClick={() => setDraftRemotes(remotes)}
            disabled={!hasChanges}
          >
            Reset
          </button>
          <button
            className="send-button"
            type="button"
            onClick={() => onSaveRemotes(draftRemotes)}
            disabled={!hasChanges || !!validationError}
          >
            Save remotes
          </button>
        </div>

        <p className="session-control-hint">
          {validationError ??
            "Remote ids become stable project routing keys. If you rename one later, the backend will reject the change while projects still reference the old id."}
        </p>
      </article>
    </section>
  );
}

type TelegramSettingsDraft = {
  enabled: boolean;
  botToken: string;
  subscribedProjectIds: string[];
  defaultProjectId: string;
  defaultSessionId: string;
};

function createTelegramDraft(status: TelegramStatusResponse | null): TelegramSettingsDraft {
  return {
    enabled: status?.enabled ?? false,
    botToken: "",
    subscribedProjectIds: status?.subscribedProjectIds ?? [],
    defaultProjectId: status?.defaultProjectId ?? "",
    defaultSessionId: status?.defaultSessionId ?? "",
  };
}

function telegramStatusLabel(status: TelegramStatusResponse | null): string {
  if (!status) {
    return "Loading";
  }
  if (status.running) {
    return "Polling";
  }
  if (status.lifecycle === "inProcess" && status.enabled && status.configured) {
    return "Stopped";
  }
  if (status.linkedChatId !== null && status.linkedChatId !== undefined) {
    return "Linked";
  }
  if (status.configured) {
    return "Configured";
  }
  return "Not configured";
}

export function TelegramPreferencesPanel({
  projects,
  sessions,
}: {
  projects: Project[];
  sessions: Session[];
}) {
  const [status, setStatus] = useState<TelegramStatusResponse | null>(null);
  const [draft, setDraft] = useState<TelegramSettingsDraft>(() => createTelegramDraft(null));
  const [isLoading, setIsLoading] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const [isTesting, setIsTesting] = useState(false);
  const [isSetupOpen, setIsSetupOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const isMountedRef = useRef(true);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    setIsLoading(true);
    fetchTelegramStatus()
      .then((nextStatus) => {
        if (cancelled) {
          return;
        }
        setStatus(nextStatus);
        setDraft(createTelegramDraft(nextStatus));
        setError(null);
      })
      .catch((loadError: unknown) => {
        if (!cancelled) {
          setError(loadError instanceof Error ? loadError.message : "Failed to load Telegram settings.");
        }
      })
      .finally(() => {
        if (!cancelled) {
          setIsLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, []);

  const projectOptions = useMemo<ComboboxOption[]>(
    () => [
      { label: "No default project", value: "" },
      ...projects.map((project) => ({
        label: project.name,
        value: project.id,
        description: project.rootPath,
      })),
    ],
    [projects],
  );
  const defaultProjectSessions = useMemo(
    () =>
      sessions.filter(
        (session) =>
          draft.defaultProjectId &&
          session.projectId === draft.defaultProjectId,
      ),
    [draft.defaultProjectId, sessions],
  );
  const sessionOptions = useMemo<ComboboxOption[]>(
    () => [
      { label: "No default session", value: "" },
      ...defaultProjectSessions.map((session) => ({
        label: session.name,
        value: session.id,
        description: `${session.agent} - ${session.status}`,
      })),
    ],
    [defaultProjectSessions],
  );
  const subscribedProjectIds = useMemo(
    () => new Set(draft.subscribedProjectIds),
    [draft.subscribedProjectIds],
  );
  const selectedDefaultSessionExists =
    draft.defaultSessionId === "" ||
    defaultProjectSessions.some((session) => session.id === draft.defaultSessionId);
  const canTestToken = draft.botToken.trim().length > 0 || Boolean(status?.botTokenMasked);
  const hasSavedToken = Boolean(status?.botTokenMasked);

  function updateDraft(patch: Partial<TelegramSettingsDraft>) {
    setDraft((current) => ({ ...current, ...patch }));
    setNotice(null);
    setError(null);
  }

  function toggleProject(projectId: string, checked: boolean) {
    setDraft((current) => {
      const nextIds = checked
        ? [...current.subscribedProjectIds, projectId]
        : current.subscribedProjectIds.filter((candidate) => candidate !== projectId);
      const removesDefaultProject = !checked && current.defaultProjectId === projectId;
      return {
        ...current,
        subscribedProjectIds: Array.from(new Set(nextIds)),
        defaultProjectId: removesDefaultProject ? "" : current.defaultProjectId,
        defaultSessionId: removesDefaultProject ? "" : current.defaultSessionId,
      };
    });
    setNotice(null);
    setError(null);
  }

  async function handleSave() {
    setIsSaving(true);
    setNotice(null);
    setError(null);
    try {
      const nextProjectIds = draft.defaultProjectId
        ? Array.from(new Set([...draft.subscribedProjectIds, draft.defaultProjectId]))
        : draft.subscribedProjectIds;
      const nextStatus = await updateTelegramConfig({
        enabled: draft.enabled,
        botToken: draft.botToken.trim() ? draft.botToken.trim() : undefined,
        subscribedProjectIds: nextProjectIds,
        defaultProjectId: draft.defaultProjectId || null,
        defaultSessionId:
          draft.defaultSessionId && selectedDefaultSessionExists
            ? draft.defaultSessionId
            : null,
      });
      if (!isMountedRef.current) {
        return;
      }
      setStatus(nextStatus);
      setDraft(createTelegramDraft(nextStatus));
      setNotice("Telegram settings saved.");
    } catch (saveError: unknown) {
      if (isMountedRef.current) {
        setError(
          saveError instanceof Error
            ? saveError.message
            : "Failed to save Telegram settings.",
        );
      }
    } finally {
      if (isMountedRef.current) {
        setIsSaving(false);
      }
    }
  }

  async function handleTestConnection() {
    setIsTesting(true);
    setNotice(null);
    setError(null);
    try {
      const trimmedToken = draft.botToken.trim();
      const result = await testTelegramConnection(
        trimmedToken ? { botToken: trimmedToken } : { useSavedToken: true },
      );
      if (!isMountedRef.current) {
        return;
      }
      setNotice(
        result.botUsername
          ? `Connected to @${result.botUsername}.`
          : `Connected to ${result.botName}.`,
      );
    } catch (testError: unknown) {
      if (isMountedRef.current) {
        setError(
          testError instanceof Error
            ? testError.message
            : "Telegram connection test failed.",
        );
      }
    } finally {
      if (isMountedRef.current) {
        setIsTesting(false);
      }
    }
  }

  async function handleRemoveToken() {
    setIsSaving(true);
    setNotice(null);
    setError(null);
    try {
      const nextStatus = await updateTelegramConfig({
        enabled: false,
        botToken: null,
      });
      if (!isMountedRef.current) {
        return;
      }
      setStatus(nextStatus);
      setDraft(createTelegramDraft(nextStatus));
      setNotice("Telegram bot token removed.");
    } catch (removeError: unknown) {
      if (isMountedRef.current) {
        setError(
          removeError instanceof Error
            ? removeError.message
            : "Failed to remove Telegram bot token.",
        );
      }
    } finally {
      if (isMountedRef.current) {
        setIsSaving(false);
      }
    }
  }

  return (
    <section className="settings-panel-stack">
      <article className="message-card prompt-settings-card telegram-settings-card">
        <div className="telegram-settings-header">
          <div>
            <div className="card-label">Mobile</div>
            <h3>Telegram</h3>
          </div>
          <span className="remote-settings-badge">{telegramStatusLabel(status)}</span>
        </div>

        <div className="prompt-settings-grid telegram-settings-grid">
          <div className="session-control-group telegram-token-group">
            <label className="session-control-label" htmlFor="telegram-bot-token">
              Bot token
            </label>
            <input
              id="telegram-bot-token"
              className="themed-input"
              type="password"
              value={draft.botToken}
              placeholder={status?.botTokenMasked ?? "BotFather token"}
              disabled={isLoading || isSaving}
              autoComplete="off"
              onChange={(event) => updateDraft({ botToken: event.target.value })}
            />
            {hasSavedToken ? (
              <p className="session-control-hint">Saved as {status?.botTokenMasked}.</p>
            ) : null}
          </div>

          <label className="remote-settings-toggle telegram-enable-toggle">
            <input
              type="checkbox"
              checked={draft.enabled}
              disabled={isLoading || isSaving}
              onChange={(event) => updateDraft({ enabled: event.target.checked })}
            />
            <span>Enable relay</span>
          </label>

          <div className="session-control-group">
            <label className="session-control-label" htmlFor="telegram-default-project">
              Default project
            </label>
            <ThemedCombobox
              id="telegram-default-project"
              value={draft.defaultProjectId}
              options={projectOptions}
              disabled={isLoading || isSaving || projects.length === 0}
              onChange={(nextProjectId) =>
                updateDraft({
                  defaultProjectId: nextProjectId,
                  defaultSessionId: "",
                  subscribedProjectIds: nextProjectId
                    ? Array.from(new Set([...draft.subscribedProjectIds, nextProjectId]))
                    : draft.subscribedProjectIds,
                })
              }
            />
          </div>

          <div className="session-control-group">
            <label className="session-control-label" htmlFor="telegram-default-session">
              Default session
            </label>
            <ThemedCombobox
              id="telegram-default-session"
              value={selectedDefaultSessionExists ? draft.defaultSessionId : ""}
              options={sessionOptions}
              disabled={
                isLoading ||
                isSaving ||
                !draft.defaultProjectId ||
                defaultProjectSessions.length === 0
              }
              onChange={(nextSessionId) => updateDraft({ defaultSessionId: nextSessionId })}
            />
          </div>
        </div>

        <div className="telegram-project-list" aria-label="Telegram subscribed projects">
          {projects.length > 0 ? (
            projects.map((project) => (
              <label key={project.id} className="telegram-project-option">
                <input
                  type="checkbox"
                  checked={subscribedProjectIds.has(project.id)}
                  disabled={isLoading || isSaving}
                  onChange={(event) => toggleProject(project.id, event.target.checked)}
                />
                <span>
                  <strong>{project.name}</strong>
                  <span>{project.rootPath}</span>
                </span>
              </label>
            ))
          ) : (
            <p className="session-control-hint">No projects yet.</p>
          )}
        </div>

        <div className="remote-settings-actions telegram-settings-actions">
          <button
            className="ghost-button"
            type="button"
            aria-expanded={isSetupOpen}
            aria-controls="telegram-setup-panel"
            onClick={() => setIsSetupOpen((current) => !current)}
          >
            {isSetupOpen ? "Hide setup" : "Setup"}
          </button>
          <button
            className="ghost-button"
            type="button"
            onClick={handleTestConnection}
            disabled={isLoading || isTesting || !canTestToken}
          >
            {isTesting ? "Testing..." : "Test connection"}
          </button>
          <button
            className="ghost-button"
            type="button"
            onClick={handleRemoveToken}
            disabled={isLoading || isSaving || !hasSavedToken}
          >
            Remove token
          </button>
          <button
            className="ghost-button"
            type="button"
            disabled
            title="Manual chat linking is active; the link-code wizard lands in a later Telegram phase."
          >
            Link chat
          </button>
          <button
            className="send-button"
            type="button"
            onClick={handleSave}
            disabled={isLoading || isSaving}
          >
            {isSaving ? "Saving..." : "Save Telegram"}
          </button>
        </div>

        {isSetupOpen ? (
          <div id="telegram-setup-panel" className="telegram-setup-panel">
            <p className="session-control-label">Setup flow</p>
            <ol>
              <li>Open @BotFather in Telegram, create a bot with /newbot, and copy the token.</li>
              <li>Paste the token here, test the connection, choose projects, then save.</li>
              <li>Turn on Enable relay. TermAl will run the Telegram relay from the main backend process.</li>
              <li>Open the bot in Telegram and send /start from the linked chat.</li>
            </ol>
            <p className="session-control-hint">
              No separate TermAl process or telegram command is required. Use /projects and /project &lt;id&gt; in Telegram to switch projects, then /sessions and /session &lt;id&gt; to switch sessions.
            </p>
          </div>
        ) : null}

        {status?.linkedChatId !== null && status?.linkedChatId !== undefined ? (
          <p className="session-control-hint">Linked chat id: {status.linkedChatId}</p>
        ) : null}
        {status?.enabled && status.lifecycle === "manual" ? (
          <p className="session-control-hint">
            Relay startup from this toggle lands in the next backend lifecycle phase.
          </p>
        ) : null}
        {notice ? <p className="session-control-hint telegram-settings-notice">{notice}</p> : null}
        {error ? <p className="session-control-hint telegram-settings-error">{error}</p> : null}
      </article>
    </section>
  );
}
export function ClaudeApprovalsPreferencesPanel({
  defaultClaudeApprovalMode,
  defaultClaudeEffort,
  defaultClaudeModel,
  onSelectEffort,
  onSelectModel,
  onSelectMode,
}: {
  defaultClaudeApprovalMode: ClaudeApprovalMode;
  defaultClaudeEffort: ClaudeEffortLevel;
  defaultClaudeModel: string;
  onSelectEffort: (effort: ClaudeEffortLevel) => void;
  onSelectModel: (model: string) => void;
  onSelectMode: (mode: ClaudeApprovalMode) => void;
}) {
  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">New Claude sessions</p>
          <p className="settings-panel-copy">
            Choose the default Claude model, mode, and effort for sessions created in this window.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Claude startup settings</h3>
        <div className="prompt-settings-grid">
          <AgentDefaultModelControl
            agent="Claude"
            id="default-claude-model"
            value={defaultClaudeModel}
            onChange={onSelectModel}
          />
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-claude-approval-mode">
              Default Claude mode
            </label>
            <ThemedCombobox
              id="default-claude-approval-mode"
              className="prompt-settings-select"
              value={defaultClaudeApprovalMode}
              options={CLAUDE_APPROVAL_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectMode(nextValue as ClaudeApprovalMode)}
            />
          </div>
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-claude-effort">
              Default Claude effort
            </label>
            <ThemedCombobox
              id="default-claude-effort"
              className="prompt-settings-select"
              value={defaultClaudeEffort}
              options={CLAUDE_EFFORT_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectEffort(nextValue as ClaudeEffortLevel)}
            />
          </div>
          <p className="session-control-hint">
            Ask keeps approval cards, Auto-approve continues through tool requests, and Plan keeps
            Claude read-only. Model settings are used when a new Claude session or delegation
            starts. Existing sessions keep their current model, mode, and effort.
          </p>
        </div>
      </article>
    </section>
  );
}

export function CodexPromptPreferencesPanel({
  defaultApprovalPolicy,
  defaultModel,
  defaultReasoningEffort,
  defaultSandboxMode,
  onSelectApprovalPolicy,
  onSelectModel,
  onSelectReasoningEffort,
  onSelectSandboxMode,
}: {
  defaultApprovalPolicy: ApprovalPolicy;
  defaultModel: string;
  defaultReasoningEffort: CodexReasoningEffort;
  defaultSandboxMode: SandboxMode;
  onSelectApprovalPolicy: (policy: ApprovalPolicy) => void;
  onSelectModel: (model: string) => void;
  onSelectReasoningEffort: (effort: CodexReasoningEffort) => void;
  onSelectSandboxMode: (mode: SandboxMode) => void;
}) {
  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">New Codex sessions</p>
          <p className="settings-panel-copy">
            Choose the default model, sandbox, approval policy, and reasoning effort for Codex
            sessions created in this window.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Codex prompt settings</h3>
        <div className="prompt-settings-grid">
          <AgentDefaultModelControl
            agent="Codex"
            id="default-codex-model"
            value={defaultModel}
            onChange={onSelectModel}
          />
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-codex-sandbox-mode">
              Default sandbox
            </label>
            <ThemedCombobox
              id="default-codex-sandbox-mode"
              className="prompt-settings-select"
              value={defaultSandboxMode}
              options={SANDBOX_MODE_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectSandboxMode(nextValue as SandboxMode)}
            />
          </div>
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-codex-approval-policy">
              Default approval policy
            </label>
            <ThemedCombobox
              id="default-codex-approval-policy"
              className="prompt-settings-select"
              value={defaultApprovalPolicy}
              options={APPROVAL_POLICY_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectApprovalPolicy(nextValue as ApprovalPolicy)}
            />
          </div>
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-codex-reasoning-effort">
              Default reasoning effort
            </label>
            <ThemedCombobox
              id="default-codex-reasoning-effort"
              className="prompt-settings-select"
              value={defaultReasoningEffort}
              options={CODEX_REASONING_EFFORT_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectReasoningEffort(nextValue as CodexReasoningEffort)}
            />
          </div>
          <p className="session-control-hint">
            Model settings are used when a new Codex session or delegation starts. Existing
            sessions keep their current model and prompt settings.
          </p>
        </div>
      </article>
    </section>
  );
}

export function CursorPreferencesPanel({
  defaultCursorModel,
  defaultCursorMode,
  onSelectModel,
  onSelectMode,
}: {
  defaultCursorModel: string;
  defaultCursorMode: CursorMode;
  onSelectModel: (model: string) => void;
  onSelectMode: (mode: CursorMode) => void;
}) {
  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">New Cursor sessions</p>
          <p className="settings-panel-copy">
            Choose the default Cursor model and mode for sessions created in this window.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Cursor startup settings</h3>
        <div className="prompt-settings-grid">
          <AgentDefaultModelControl
            agent="Cursor"
            id="default-cursor-model"
            value={defaultCursorModel}
            onChange={onSelectModel}
          />
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-cursor-mode">
              Default Cursor mode
            </label>
            <ThemedCombobox
              id="default-cursor-mode"
              className="prompt-settings-select"
              value={defaultCursorMode}
              options={CURSOR_MODE_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectMode(nextValue as CursorMode)}
            />
          </div>
          <p className="session-control-hint">
            Model settings are used when a new Cursor session or delegation starts. Existing
            sessions keep their current model and mode.
          </p>
        </div>
      </article>
    </section>
  );
}

export function GeminiPreferencesPanel({
  defaultGeminiApprovalMode,
  defaultGeminiModel,
  onSelectApprovalMode,
  onSelectModel,
}: {
  defaultGeminiApprovalMode: GeminiApprovalMode;
  defaultGeminiModel: string;
  onSelectApprovalMode: (mode: GeminiApprovalMode) => void;
  onSelectModel: (model: string) => void;
}) {
  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">New Gemini sessions</p>
          <p className="settings-panel-copy">
            Choose the default Gemini model and approval mode for sessions created in this window.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Gemini startup settings</h3>
        <div className="prompt-settings-grid">
          <AgentDefaultModelControl
            agent="Gemini"
            id="default-gemini-model"
            value={defaultGeminiModel}
            onChange={onSelectModel}
          />
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-gemini-approval-mode">
              Default Gemini approvals
            </label>
            <ThemedCombobox
              id="default-gemini-approval-mode"
              className="prompt-settings-select"
              value={defaultGeminiApprovalMode}
              options={GEMINI_APPROVAL_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectApprovalMode(nextValue as GeminiApprovalMode)}
            />
          </div>
          <p className="session-control-hint">
            Model settings are used when a new Gemini session or delegation starts. Existing
            sessions keep their current model and approval mode.
          </p>
        </div>
      </article>
    </section>
  );
}

export function ThemedCombobox({
  "aria-label": ariaLabel,
  "aria-labelledby": ariaLabelledBy,
  className,
  disabled = false,
  id,
  onChange,
  options,
  value,
}: {
  "aria-label"?: string;
  "aria-labelledby"?: string;
  className?: string;
  disabled?: boolean;
  id: string;
  onChange: (nextValue: string) => void;
  options: readonly ComboboxOption[];
  value: string;
}) {
  const listboxId = useId();
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(() =>
    Math.max(
      options.findIndex((option) => option.value === value),
      0,
    ),
  );
  const [menuStyle, setMenuStyle] = useState<CSSProperties | null>(null);

  const selectedIndex = options.findIndex((option) => option.value === value);
  const safeSelectedIndex = selectedIndex >= 0 ? selectedIndex : 0;
  const selectedOption = options[safeSelectedIndex] ?? options[0];

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    setActiveIndex(safeSelectedIndex);
  }, [isOpen, safeSelectedIndex]);

  useLayoutEffect(() => {
    if (!isOpen || !menuStyle) {
      return;
    }

    const listbox = listRef.current;
    if (!listbox) {
      return;
    }

    const activeOption = listbox.querySelector<HTMLElement>(`[data-option-index="${activeIndex}"]`);
    if (!activeOption) {
      return;
    }

    const listRect = listbox.getBoundingClientRect();
    const optionRect = activeOption.getBoundingClientRect();

    if (optionRect.top < listRect.top) {
      listbox.scrollTop += optionRect.top - listRect.top;
    } else if (optionRect.bottom > listRect.bottom) {
      listbox.scrollTop += optionRect.bottom - listRect.bottom;
    }
  }, [activeIndex, isOpen, menuStyle]);

  useLayoutEffect(() => {
    if (!isOpen) {
      return;
    }

    function updateMenuStyle() {
      const trigger = triggerRef.current;
      if (!trigger) {
        return;
      }

      const rect = trigger.getBoundingClientRect();
      const viewportPadding = 12;
      const estimatedHeight = Math.min(Math.max(options.length * 76 + 12, 120), 360);
      const availableBelow = window.innerHeight - rect.bottom - viewportPadding;
      const availableAbove = rect.top - viewportPadding;
      const openUpward =
        availableBelow < Math.min(estimatedHeight, 220) && availableAbove > availableBelow;
      const maxHeight = Math.max(openUpward ? availableAbove : availableBelow, 140);

      setMenuStyle({
        left: rect.left,
        width: rect.width,
        maxHeight,
        top: openUpward ? undefined : rect.bottom + 8,
        bottom: openUpward ? window.innerHeight - rect.top + 8 : undefined,
      });
    }

    updateMenuStyle();
    window.addEventListener("resize", updateMenuStyle);
    window.addEventListener("scroll", updateMenuStyle, true);

    return () => {
      window.removeEventListener("resize", updateMenuStyle);
      window.removeEventListener("scroll", updateMenuStyle, true);
    };
  }, [isOpen, options.length]);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    function handlePointerDown(event: PointerEvent) {
      const target = event.target;
      if (!(target instanceof Node)) {
        return;
      }

      if (triggerRef.current?.contains(target) || listRef.current?.contains(target)) {
        return;
      }

      setIsOpen(false);
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setIsOpen(false);
        triggerRef.current?.focus();
        return;
      }

      if (event.key === "Tab") {
        setIsOpen(false);
        return;
      }

      if (event.key === "ArrowDown") {
        event.preventDefault();
        setActiveIndex((current) => (current + 1) % options.length);
        return;
      }

      if (event.key === "ArrowUp") {
        event.preventDefault();
        setActiveIndex((current) => (current - 1 + options.length) % options.length);
        return;
      }

      if (event.key === "Home") {
        event.preventDefault();
        setActiveIndex(0);
        return;
      }

      if (event.key === "End") {
        event.preventDefault();
        setActiveIndex(options.length - 1);
        return;
      }

      if (event.key === " " || event.key === "Enter") {
        event.preventDefault();
        const nextOption = options[activeIndex];
        if (!nextOption) {
          return;
        }

        onChange(nextOption.value);
        if (event.key === "Enter") {
          setIsOpen(false);
          triggerRef.current?.focus();
        }
      }
    }

    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);

    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [activeIndex, isOpen, onChange, options]);

  function handleTriggerKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>) {
    if (disabled) {
      return;
    }

    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIndex(safeSelectedIndex);
      setIsOpen(true);
      return;
    }

    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (!isOpen) {
        setActiveIndex(safeSelectedIndex);
        setIsOpen(true);
      }
    }
  }

  return (
    <>
      <button
        ref={triggerRef}
        id={id}
        className={`session-select combo-trigger ${className ?? ""}`.trim()}
        type="button"
        role="combobox"
        aria-label={ariaLabel}
        aria-controls={listboxId}
        aria-expanded={isOpen}
        aria-haspopup="listbox"
        aria-labelledby={ariaLabelledBy}
        aria-activedescendant={isOpen ? `${listboxId}-option-${activeIndex}` : undefined}
        disabled={disabled}
        onClick={() => {
          if (!disabled) {
            setActiveIndex(safeSelectedIndex);
            setIsOpen((current) => !current);
          }
        }}
        onKeyDown={handleTriggerKeyDown}
      >
        <span className="combo-trigger-value">{selectedOption?.label ?? value}</span>
        <span className={`combo-trigger-caret ${isOpen ? "open" : ""}`} aria-hidden="true">
          v
        </span>
      </button>

      {isOpen && menuStyle
        ? createPortal(
            <div
              ref={listRef}
              id={listboxId}
              className="combo-menu"
              role="listbox"
              style={menuStyle}
            >
              {options.map((option, index) => {
                const isSelected = option.value === value;
                const isActive = index === activeIndex;

                return (
                  <button
                    key={option.value}
                    id={`${listboxId}-option-${index}`}
                    className={`combo-option ${isActive ? "active" : ""} ${isSelected ? "selected" : ""}`}
                    type="button"
                    role="option"
                    aria-selected={isSelected}
                    data-option-index={index}
                    onMouseEnter={() => {
                      setActiveIndex(index);
                    }}
                    onClick={() => {
                      onChange(option.value);
                      setIsOpen(false);
                      triggerRef.current?.focus();
                    }}
                  >
                    <span className="combo-option-copy">
                      <span className="combo-option-label">{option.label}</span>
                      {option.description ? (
                        <span className="combo-option-description">{option.description}</span>
                      ) : null}
                      {option.badges?.length ? (
                        <span className="combo-option-badges">
                          {option.badges.map((badge) => (
                            <span key={badge} className="combo-option-badge">
                              {badge}
                            </span>
                          ))}
                        </span>
                      ) : null}
                    </span>
                    <span
                      className={`combo-option-indicator ${isSelected ? "visible" : ""}`}
                      aria-hidden="true"
                    />
                  </button>
                );
              })}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}
