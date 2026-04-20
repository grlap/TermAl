// Error boundary for Monaco inline-zone portal children.
//
// What this file owns:
//   - `InlineZoneErrorBoundary` — a small class component (the only
//     React API that can catch render errors from its subtree) that
//     wraps the node a MonacoCodeEditor inline zone renders through
//     `createPortal`. If the zone's render callback throws — common
//     failure modes: a malformed Mermaid fence that slips past the
//     render-time parser, a KaTeX parse error that escapes
//     `throwOnError: false`, a thrown synchronous error inside
//     MarkdownContent's component tree — the boundary catches it,
//     logs it, and renders a compact fallback notice so the host
//     editor (Monaco itself) stays mounted with the user's buffer
//     intact. Without this boundary, a single bad fence in a file
//     would unmount MonacoCodeEditor entirely and lose unsaved
//     edits — see docs/bugs.md → "Missing error boundary around
//     portal render() in MonacoCodeEditor".
//   - `INLINE_ZONE_ERROR_FALLBACK_TEXT` — the fallback copy,
//     exported so tests can assert it without duplicating strings.
//
// Reset semantics: when `zoneId` changes, the boundary's internal
// error state is discarded so the new zone gets a fresh render
// attempt. This is how React-style "reset by key" works on a class
// component: we read `zoneId` in `componentDidUpdate` and clear
// `hasError` when it moved. The caller (MonacoCodeEditor) passes
// the zone's stable id as `zoneId`; zones whose content changed
// but whose id stayed the same intentionally remain in the error
// state until the caller generates a new id (or the user scrolls
// the zone out of view and the portal unmounts).
//
// What this file does NOT own:
//   - Monaco view-zone lifecycle — `MonacoCodeEditor` manages the
//     zone add/remove, height measurement, and ResizeObserver
//     wiring. This component sits inside the portal target.
//   - Error reporting beyond a `console.error` for the dev
//     console. The project has no error-telemetry sink yet; when
//     one lands, extend `componentDidCatch` to forward to it.

import { Component, type ErrorInfo, type ReactNode } from "react";

export const INLINE_ZONE_ERROR_FALLBACK_TEXT =
  "Diagram failed to render — view the source below for details.";

export type InlineZoneErrorBoundaryProps = {
  children: ReactNode;
  zoneId: string;
};

type InlineZoneErrorBoundaryState = {
  hasError: boolean;
};

export class InlineZoneErrorBoundary extends Component<
  InlineZoneErrorBoundaryProps,
  InlineZoneErrorBoundaryState
> {
  state: InlineZoneErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): InlineZoneErrorBoundaryState {
    // React's error-boundary contract: return the next state on a
    // render error. We just flip `hasError` — the specific error
    // object goes to `componentDidCatch` below for logging.
    return { hasError: true };
  }

  componentDidUpdate(previousProps: InlineZoneErrorBoundaryProps) {
    if (this.state.hasError && previousProps.zoneId !== this.props.zoneId) {
      // New zone id → discard the old error state so the new
      // render gets a fresh attempt. Previous zone's children
      // have already unmounted (portal re-keyed in
      // MonacoCodeEditor) so there's no stale subtree to worry
      // about.
      this.setState({ hasError: false });
    }
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    // Log once to the dev console so the failure is diagnosable
    // when the fallback copy appears in a user's editor. When a
    // structured telemetry sink lands, forward here.
    // eslint-disable-next-line no-console
    console.error(
      "InlineZoneErrorBoundary caught error rendering inline zone",
      this.props.zoneId,
      error,
      errorInfo.componentStack,
    );
  }

  render() {
    if (this.state.hasError) {
      return (
        <div
          className="monaco-inline-zone-error"
          role="status"
          aria-live="polite"
        >
          {INLINE_ZONE_ERROR_FALLBACK_TEXT}
        </div>
      );
    }
    return this.props.children;
  }
}
