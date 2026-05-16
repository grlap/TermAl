// Owns: viewport-gated activation wrapper for expensive message-card regions.
// Does not own: markdown, syntax highlighting, or message-card rendering.
// Split from: ui/src/message-cards.tsx.

import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";

import { useDeferredHeavyContentActivation } from "./deferred-heavy-content-activation";
import {
  DEFERRED_RENDER_RESUME_EVENT,
  DEFERRED_RENDER_ROOT_MARGIN_PX,
  isDeferredRenderActivationSuspended,
  isElementNearRenderViewport,
  resolveDeferredRenderRoot,
} from "./deferred-render";

export function DeferredHeavyContent({
  children,
  estimatedHeight,
  placeholder,
  preferImmediateRender = false,
}: {
  children: ReactNode;
  estimatedHeight: number;
  placeholder: ReactNode;
  preferImmediateRender?: boolean;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const allowDeferredActivation = useDeferredHeavyContentActivation();
  const [isActivated, setIsActivated] = useState(() => preferImmediateRender);
  const shouldRenderContent = preferImmediateRender || isActivated;

  // Already-near content must activate before paint; the observer path below
  // still uses rAF to batch later scroll/viewport-triggered activations.
  useLayoutEffect(() => {
    if (shouldRenderContent || !allowDeferredActivation) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    const root = resolveDeferredRenderRoot(node);
    if (isDeferredRenderActivationSuspended(root)) {
      return;
    }
    if (
      isElementNearRenderViewport(node, root, DEFERRED_RENDER_ROOT_MARGIN_PX)
    ) {
      setIsActivated(true);
    }
  }, [allowDeferredActivation, shouldRenderContent]);

  useEffect(() => {
    if (shouldRenderContent || !allowDeferredActivation) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    const root = resolveDeferredRenderRoot(node);
    let activationFrameId: number | null = null;
    const activate = () => {
      if (activationFrameId !== null) {
        return;
      }
      if (isDeferredRenderActivationSuspended(root)) {
        return;
      }
      activationFrameId = window.requestAnimationFrame(() => {
        activationFrameId = null;
        if (isDeferredRenderActivationSuspended(root)) {
          return;
        }
        setIsActivated(true);
      });
    };
    const activateIfNearViewport = () => {
      if (
        isElementNearRenderViewport(
          node,
          root,
          DEFERRED_RENDER_ROOT_MARGIN_PX,
        )
      ) {
        activate();
      }
    };
    root?.addEventListener(DEFERRED_RENDER_RESUME_EVENT, activateIfNearViewport);

    if (
      typeof window === "undefined" ||
      typeof window.IntersectionObserver === "undefined"
    ) {
      activate();
      return () => {
        if (activationFrameId !== null) {
          window.cancelAnimationFrame(activationFrameId);
        }
        root?.removeEventListener(
          DEFERRED_RENDER_RESUME_EVENT,
          activateIfNearViewport,
        );
      };
    }

    const observer = new window.IntersectionObserver(
      (entries) => {
        if (
          entries.some(
            (entry) => entry.isIntersecting || entry.intersectionRatio > 0,
          )
        ) {
          activate();
        }
      },
      {
        root,
        rootMargin: `${DEFERRED_RENDER_ROOT_MARGIN_PX}px 0px ${DEFERRED_RENDER_ROOT_MARGIN_PX}px 0px`,
        threshold: 0.01,
      },
    );

    observer.observe(node);
    return () => {
      if (activationFrameId !== null) {
        window.cancelAnimationFrame(activationFrameId);
      }
      root?.removeEventListener(
        DEFERRED_RENDER_RESUME_EVENT,
        activateIfNearViewport,
      );
      observer.disconnect();
    };
  }, [allowDeferredActivation, shouldRenderContent]);

  return (
    <div
      ref={containerRef}
      className="deferred-heavy-content"
      style={
        shouldRenderContent
          ? undefined
          : ({
              "--deferred-min-height": `${estimatedHeight}px`,
            } as CSSProperties)
      }
    >
      {shouldRenderContent ? children : placeholder}
    </div>
  );
}
