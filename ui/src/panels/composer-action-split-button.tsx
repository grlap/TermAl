// Owns the accessible split-button interaction for choosing and invoking a
// composer action. Deliberately does not own drafts, sending, or delegation;
// those remain in AgentSessionPanel.composer.tsx.

import {
  forwardRef,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { createPortal } from "react-dom";

export type ComposerActionMode = "send" | "reviewer" | "explorer";

export type ComposerActionOption = {
  disabled?: boolean;
  label: string;
  mode: ComposerActionMode;
};

function enabledOptionIndex(
  options: readonly ComposerActionOption[],
  startIndex: number,
  direction: 1 | -1,
): number {
  if (options.length === 0) {
    return -1;
  }

  for (let offset = 0; offset < options.length; offset += 1) {
    const index =
      (startIndex + direction * offset + options.length) % options.length;
    if (!options[index]?.disabled) {
      return index;
    }
  }

  return -1;
}

export const ComposerActionSplitButton = forwardRef<
  HTMLButtonElement,
  {
    actionLabel: string;
    actionTitle?: string;
    className?: string;
    disabled: boolean;
    menuDisabled?: boolean;
    onAction: () => void;
    onModeChange: (mode: ComposerActionMode) => void;
    options: readonly ComposerActionOption[];
    primaryClassName?: string;
    selectedMode: ComposerActionMode;
  }
>(function ComposerActionSplitButton(
  {
    actionLabel,
    actionTitle,
    className,
    disabled,
    menuDisabled = false,
    onAction,
    onModeChange,
    options,
    primaryClassName,
    selectedMode,
  },
  primaryButtonRef,
) {
  const menuId = useId();
  const groupRef = useRef<HTMLDivElement | null>(null);
  const menuTriggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const [menuStyle, setMenuStyle] = useState<CSSProperties | null>(null);

  const selectedIndex = options.findIndex(
    (option) => option.mode === selectedMode && !option.disabled,
  );

  function initialActiveIndex() {
    return selectedIndex >= 0
      ? selectedIndex
      : enabledOptionIndex(options, 0, 1);
  }

  function openMenu() {
    if (menuDisabled) {
      return;
    }
    setActiveIndex(initialActiveIndex());
    setIsOpen(true);
  }

  function closeMenu({ restoreFocus }: { restoreFocus: boolean }) {
    setIsOpen(false);
    setMenuStyle(null);
    if (restoreFocus) {
      menuTriggerRef.current?.focus();
    }
  }

  function chooseOption(index: number) {
    const option = options[index];
    if (!option || option.disabled) {
      return;
    }
    onModeChange(option.mode);
    closeMenu({ restoreFocus: true });
  }

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    function handlePointerDown(event: PointerEvent) {
      const target = event.target;
      if (!(target instanceof Node)) {
        return;
      }
      if (groupRef.current?.contains(target) || menuRef.current?.contains(target)) {
        return;
      }
      closeMenu({ restoreFocus: false });
    }

    document.addEventListener("pointerdown", handlePointerDown);
    return () => document.removeEventListener("pointerdown", handlePointerDown);
  }, [isOpen]);

  useLayoutEffect(() => {
    if (!isOpen) {
      return;
    }

    function updateMenuPosition() {
      const trigger = menuTriggerRef.current;
      const group = groupRef.current;
      if (!trigger || !group) {
        return;
      }

      const groupRect = group.getBoundingClientRect();
      const viewportPadding = 12;
      const estimatedHeight = options.length * 44 + 12;
      const availableAbove = groupRect.top - viewportPadding;
      const availableBelow = window.innerHeight - groupRect.bottom - viewportPadding;
      const openUpward = availableAbove >= estimatedHeight || availableAbove > availableBelow;

      setMenuStyle({
        position: "fixed",
        right: Math.max(window.innerWidth - groupRect.right, viewportPadding),
        width: Math.max(groupRect.width, 210),
        maxHeight: Math.max(openUpward ? availableAbove : availableBelow, 120),
        top: openUpward ? undefined : groupRect.bottom + 7,
        bottom: openUpward ? window.innerHeight - groupRect.top + 7 : undefined,
      });
    }

    updateMenuPosition();
    window.addEventListener("resize", updateMenuPosition);
    window.addEventListener("scroll", updateMenuPosition, true);
    return () => {
      window.removeEventListener("resize", updateMenuPosition);
      window.removeEventListener("scroll", updateMenuPosition, true);
    };
  }, [isOpen, options.length]);

  useLayoutEffect(() => {
    if (!isOpen || !menuStyle || activeIndex < 0) {
      return;
    }
    itemRefs.current[activeIndex]?.focus();
  }, [activeIndex, isOpen, menuStyle]);

  function handleTriggerKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>) {
    if (menuDisabled) {
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      openMenu();
    }
  }

  function handleMenuKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeMenu({ restoreFocus: true });
      return;
    }
    if (event.key === "Tab") {
      closeMenu({ restoreFocus: false });
      return;
    }
    if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      const startIndex = event.key === "Home" ? 0 : options.length - 1;
      setActiveIndex(
        enabledOptionIndex(options, startIndex, event.key === "Home" ? 1 : -1),
      );
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const direction = event.key === "ArrowDown" ? 1 : -1;
      setActiveIndex((current) =>
        enabledOptionIndex(options, current + direction, direction),
      );
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      chooseOption(activeIndex);
    }
  }

  return (
    <div
      ref={groupRef}
      className={`composer-action-split ${className ?? ""}`.trim()}
      role="group"
      aria-label="Composer action"
    >
      <button
        ref={primaryButtonRef}
        className={`send-button composer-action-primary ${primaryClassName ?? ""}`.trim()}
        type="button"
        title={actionTitle}
        onMouseDown={(event) => event.preventDefault()}
        onClick={onAction}
        onKeyDown={handleTriggerKeyDown}
        disabled={disabled}
      >
        {actionLabel}
      </button>
      <button
        ref={menuTriggerRef}
        className="send-button composer-action-menu-trigger"
        type="button"
        aria-controls={menuId}
        aria-expanded={isOpen}
        aria-haspopup="menu"
        aria-label={`Choose composer action, current: ${actionLabel}`}
        disabled={menuDisabled}
        onClick={() => (isOpen ? closeMenu({ restoreFocus: false }) : openMenu())}
        onKeyDown={handleTriggerKeyDown}
      >
        <svg viewBox="0 0 12 8" aria-hidden="true">
          <path d="M1 1.25 6 6.25 11 1.25" fill="none" stroke="currentColor" strokeWidth="1.6" />
        </svg>
      </button>

      {isOpen && menuStyle
        ? createPortal(
            <div
              ref={menuRef}
              id={menuId}
              className="composer-action-menu"
              role="menu"
              aria-label="Composer action"
              style={menuStyle}
              onKeyDown={handleMenuKeyDown}
            >
              {options.map((option, index) => (
                <button
                  key={option.mode}
                  ref={(element) => {
                    itemRefs.current[index] = element;
                  }}
                  className="composer-action-menu-item"
                  type="button"
                  role="menuitemradio"
                  aria-checked={option.mode === selectedMode}
                  aria-disabled={option.disabled || undefined}
                  disabled={option.disabled}
                  tabIndex={index === activeIndex ? 0 : -1}
                  onMouseEnter={() => {
                    if (!option.disabled) {
                      setActiveIndex(index);
                    }
                  }}
                  onClick={() => chooseOption(index)}
                >
                  <span>{option.label}</span>
                  <span
                    className={`composer-action-menu-check${
                      option.mode === selectedMode ? " selected" : ""
                    }`}
                    aria-hidden="true"
                  >
                    ✓
                  </span>
                </button>
              ))}
            </div>,
            document.body,
          )
        : null}
    </div>
  );
});
