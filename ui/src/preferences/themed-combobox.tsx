// Split from ../preferences-panels.tsx to keep the preferences shell from
// owning the reusable combobox implementation.
import {
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { createPortal } from "react-dom";

import type { ComboboxOption } from "../session-model-utils";

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
  const firstEnabledIndex = options.findIndex((option) => !option.disabled);
  const lastEnabledIndex = options.reduce(
    (last, option, index) => (option.disabled ? last : index),
    -1,
  );
  const selectedIndex = options.findIndex((option) => option.value === value);
  const initialActiveIndex =
    selectedIndex >= 0 && !options[selectedIndex]?.disabled
      ? selectedIndex
      : firstEnabledIndex;
  const [activeIndex, setActiveIndex] = useState(initialActiveIndex);
  const [menuStyle, setMenuStyle] = useState<CSSProperties | null>(null);

  const selectedOption = selectedIndex >= 0 ? options[selectedIndex] : undefined;
  const activeIndexRef = useRef(activeIndex);

  function updateActiveIndex(next: number) {
    activeIndexRef.current = next;
    setActiveIndex(next);
  }

  function adjacentEnabledIndex(current: number, direction: 1 | -1) {
    if (options.length === 0 || firstEnabledIndex < 0) return -1;
    let next = current;
    for (let visited = 0; visited < options.length; visited += 1) {
      next = (next + direction + options.length) % options.length;
      if (!options[next]?.disabled) return next;
    }
    return -1;
  }

  useEffect(() => {
    if (disabled) setIsOpen(false);
  }, [disabled]);

  useEffect(() => {
    activeIndexRef.current = activeIndex;
  }, [activeIndex]);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    updateActiveIndex(initialActiveIndex);
  }, [isOpen, initialActiveIndex]);

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
      if (disabled) return;
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
        updateActiveIndex(adjacentEnabledIndex(activeIndexRef.current, 1));
        return;
      }

      if (event.key === "ArrowUp") {
        event.preventDefault();
        updateActiveIndex(adjacentEnabledIndex(activeIndexRef.current, -1));
        return;
      }

      if (event.key === "Home") {
        event.preventDefault();
        updateActiveIndex(firstEnabledIndex);
        return;
      }

      if (event.key === "End") {
        event.preventDefault();
        updateActiveIndex(lastEnabledIndex);
        return;
      }

      if (event.key === " " || event.key === "Enter") {
        event.preventDefault();
        const nextOption = options[activeIndexRef.current];
        if (!nextOption || nextOption.disabled) {
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
  }, [disabled, isOpen, onChange, options]);

  function handleTriggerKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>) {
    if (disabled) {
      return;
    }

    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!isOpen && firstEnabledIndex >= 0) {
        updateActiveIndex(initialActiveIndex);
        setIsOpen(true);
      }
      return;
    }

    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (!isOpen && firstEnabledIndex >= 0) {
        updateActiveIndex(initialActiveIndex);
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
        aria-activedescendant={isOpen && activeIndex >= 0 ? `${listboxId}-option-${activeIndex}` : undefined}
        disabled={disabled}
        onClick={() => {
          if (!disabled) {
            if (!isOpen) updateActiveIndex(initialActiveIndex);
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

      {isOpen && !disabled && menuStyle
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
                    aria-disabled={option.disabled || undefined}
                    disabled={option.disabled}
                    data-option-index={index}
                    onMouseEnter={() => {
                      if (!option.disabled) updateActiveIndex(index);
                    }}
                    onClick={() => {
                      if (disabled || option.disabled) return;
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
