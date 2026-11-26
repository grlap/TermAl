// Owns the compact delivery status, full reason and prompt disclosure.
// Queue policy and callbacks remain in AgentSessionPanel.
// Split from ui/src/panels/AgentSessionPanel.tsx.

import { useId, useLayoutEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import "./prompt-delivery-row.css";

export function PromptDeliveryRow({
  label,
  reason,
  onDismiss,
  promptContent,
  searchRevealQuery,
}: {
  label: string;
  reason: string;
  onDismiss?: () => void;
  promptContent?: ReactNode;
  searchRevealQuery: string | null;
}) {
  const promptId = useId();
  const [disclosure, setDisclosure] = useState({
    searchRevealQuery,
    expanded: searchRevealQuery !== null,
  });
  const { expanded } = disclosure;
  // The active match must be visible when the pane's layout effect scrolls to it.
  if (disclosure.searchRevealQuery !== searchRevealQuery) {
    setDisclosure({ searchRevealQuery, expanded: searchRevealQuery !== null || expanded });
  }

  const headerRef = useRef<HTMLDivElement>(null);
  const reasonRef = useRef<HTMLSpanElement>(null);
  const tooltipRef = useRef<HTMLSpanElement>(null);
  const [reasonFocused, setReasonFocused] = useState(false);
  const [reasonHovered, setReasonHovered] = useState(false);
  const showReason = reasonFocused || reasonHovered;

  useLayoutEffect(() => {
    const header = headerRef.current;
    const trigger = reasonRef.current;
    const tooltip = tooltipRef.current;
    if (!showReason || !header || !trigger || !tooltip) return;

    const scrollport = trigger.closest<HTMLElement>(".message-stack");
    const isTriggerVisible = ({ top, bottom }: DOMRect) => {
      if (!scrollport) return true;
      const clipTop = scrollport.getBoundingClientRect().top + scrollport.clientTop;
      return bottom > clipTop && top < clipTop + scrollport.clientHeight;
    };

    function dismissReason() {
      setReasonFocused(false);
      setReasonHovered(false);
    }

    const positionTooltip = () => {
      const triggerRect = trigger.getBoundingClientRect();
      if (!isTriggerVisible(triggerRect)) {
        dismissReason();
        return;
      }
      const { left, width } = header.getBoundingClientRect();
      const { top, bottom } = triggerRect;
      const roomAbove = top;
      const roomBelow = document.documentElement.clientHeight - bottom;
      tooltip.style.left = `${left}px`;
      tooltip.style.width = `${width}px`;
      const above = tooltip.scrollHeight <= roomAbove || roomAbove >= roomBelow;
      tooltip.style.maxHeight = `${Math.max(0, above ? roomAbove : roomBelow)}px`;
      tooltip.style.top = `${above ? top - tooltip.getBoundingClientRect().height : bottom}px`;
    };

    const dismissOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        // A scroll can clip the trigger before the scroll listener runs.
        if (isTriggerVisible(trigger.getBoundingClientRect())) event.stopPropagation();
        dismissReason();
      }
    };

    positionTooltip();
    const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(positionTooltip);
    resize?.observe(header);
    if (scrollport) resize?.observe(scrollport);
    window.addEventListener("resize", positionTooltip);
    window.addEventListener("scroll", positionTooltip, true);
    document.addEventListener("keydown", dismissOnEscape, true);
    return () => {
      resize?.disconnect();
      window.removeEventListener("resize", positionTooltip);
      window.removeEventListener("scroll", positionTooltip, true);
      document.removeEventListener("keydown", dismissOnEscape, true);
    };
  }, [showReason, reason]);

  function leaveReason(event: MouseEvent) {
    const next = event.relatedTarget;
    if (next instanceof Node &&
        (reasonRef.current?.contains(next) || tooltipRef.current?.contains(next))) return;
    setReasonHovered(false);
  }

  return (
    <article className="prompt-delivery-row" aria-label="Prompt delivery status">
      <div className="prompt-delivery-row-header" ref={headerRef}>
        {promptContent ? (
          <button
            className="prompt-delivery-row-toggle"
            type="button"
            aria-label={expanded ? "Hide prompt" : "Show prompt"}
            aria-expanded={expanded}
            aria-controls={promptId}
            onClick={() => setDisclosure(previous => ({ ...previous, expanded: !previous.expanded }))}
          >
            <span aria-hidden="true" />
          </button>
        ) : null}
        <strong className="prompt-delivery-row-label" title={label}>{label}</strong>
        <span
          className="prompt-delivery-row-reason"
          ref={reasonRef}
          tabIndex={0}
          onFocus={() => setReasonFocused(true)}
          onBlur={() => setReasonFocused(false)}
          onMouseEnter={() => setReasonHovered(true)}
          onMouseLeave={leaveReason}
        >
          <span className="prompt-delivery-row-reason-text">{reason}</span>
        </span>
        {onDismiss ? (
          <button
            className="queue-resume-button prompt-delivery-row-dismiss"
            type="button"
            title="Remove this row from the queue. The prompt is never sent again; its delivery record stays."
            onClick={onDismiss}
          >
            Dismiss
          </button>
        ) : null}
      </div>
      {promptContent ? (
        <div id={promptId} hidden={!expanded}>{expanded ? promptContent : null}</div>
      ) : null}
      {showReason ? createPortal(
        <span
          className="prompt-delivery-row-tooltip"
          ref={tooltipRef}
          role="tooltip"
          aria-hidden="true"
          onMouseEnter={() => setReasonHovered(true)}
          onMouseLeave={leaveReason}
        >
          {reason}
        </span>,
        document.body,
      ) : null}
    </article>
  );
}
