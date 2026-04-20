// Unit coverage for `InlineZoneErrorBoundary`. The boundary wraps
// Monaco inline-zone portal children so a thrown render (malformed
// Mermaid fence, KaTeX parse escape, etc.) does not unmount the
// whole MonacoCodeEditor and lose the user's unsaved buffer.
//
// These tests exercise the boundary directly rather than going
// through Monaco — the Monaco integration mocks the editor in
// other test files, and the boundary's contract (catch → fallback,
// reset on zoneId change, error logged once) is fully testable
// with just React + Testing Library.

import { render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  INLINE_ZONE_ERROR_FALLBACK_TEXT,
  InlineZoneErrorBoundary,
} from "./InlineZoneErrorBoundary";

function ThrowOnRender({ message }: { message: string }): ReactNode {
  throw new Error(message);
}

function Safe({ text }: { text: string }): ReactNode {
  return <p>{text}</p>;
}

describe("InlineZoneErrorBoundary", () => {
  let consoleErrorSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    // React logs a secondary "The above error occurred in ..."
    // message when an error boundary catches — silence both that
    // and our own `componentDidCatch` log so the test output
    // stays readable. We still inspect the spy for assertions
    // that care about logging.
    consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterEach(() => {
    consoleErrorSpy.mockRestore();
  });

  it("renders children normally when nothing throws", () => {
    render(
      <InlineZoneErrorBoundary zoneId="zone-1">
        <Safe text="rendered normally" />
      </InlineZoneErrorBoundary>,
    );

    expect(screen.getByText("rendered normally")).toBeInTheDocument();
    expect(screen.queryByText(INLINE_ZONE_ERROR_FALLBACK_TEXT)).toBeNull();
  });

  it("catches a render error and shows the fallback UI", () => {
    render(
      <InlineZoneErrorBoundary zoneId="zone-1">
        <ThrowOnRender message="mermaid parse failed" />
      </InlineZoneErrorBoundary>,
    );

    expect(screen.getByText(INLINE_ZONE_ERROR_FALLBACK_TEXT)).toBeInTheDocument();
    // Fallback is accessible: role=status + aria-live=polite.
    const status = screen.getByRole("status");
    expect(status).toHaveTextContent(INLINE_ZONE_ERROR_FALLBACK_TEXT);
    expect(status.getAttribute("aria-live")).toBe("polite");
  });

  it("logs caught errors with the zoneId for diagnosability", () => {
    render(
      <InlineZoneErrorBoundary zoneId="zone-42">
        <ThrowOnRender message="KaTeX parse failed" />
      </InlineZoneErrorBoundary>,
    );

    // `componentDidCatch` logs "InlineZoneErrorBoundary caught
    // error rendering inline zone <id> <error> <stack>". React
    // itself also logs a secondary error. Find our log in the
    // call list by its distinctive prefix.
    const ourCalls = consoleErrorSpy.mock.calls.filter((call: unknown[]) =>
      typeof call[0] === "string" &&
      call[0].startsWith("InlineZoneErrorBoundary caught error rendering"),
    );
    expect(ourCalls.length).toBe(1);
    expect(ourCalls[0]).toContain("zone-42");
    // The error instance is one of the args — find it.
    const loggedError = ourCalls[0].find(
      (arg: unknown) => arg instanceof Error,
    ) as Error | undefined;
    expect(loggedError?.message).toBe("KaTeX parse failed");
  });

  it("isolates failures: a sibling boundary with safe children stays rendered", () => {
    render(
      <div>
        <InlineZoneErrorBoundary zoneId="zone-bad">
          <ThrowOnRender message="mermaid parse failed" />
        </InlineZoneErrorBoundary>
        <InlineZoneErrorBoundary zoneId="zone-good">
          <Safe text="sibling zone survives" />
        </InlineZoneErrorBoundary>
      </div>,
    );

    // The bad zone shows the fallback; the good zone renders as
    // normal. This is what protects MonacoCodeEditor from losing
    // the editor when one zone throws — other zones, and the
    // Monaco editor itself, keep mounting normally.
    expect(screen.getByText(INLINE_ZONE_ERROR_FALLBACK_TEXT)).toBeInTheDocument();
    expect(screen.getByText("sibling zone survives")).toBeInTheDocument();
  });

  it("resets its error state when zoneId changes", () => {
    // Phase 1: throw, so the boundary enters error state.
    const { rerender } = render(
      <InlineZoneErrorBoundary zoneId="zone-1">
        <ThrowOnRender message="first throw" />
      </InlineZoneErrorBoundary>,
    );
    expect(screen.getByText(INLINE_ZONE_ERROR_FALLBACK_TEXT)).toBeInTheDocument();

    // Phase 2: rerender with a DIFFERENT zoneId and safe
    // children. The boundary must clear its `hasError` state in
    // `componentDidUpdate` and render the new children.
    rerender(
      <InlineZoneErrorBoundary zoneId="zone-2">
        <Safe text="new zone renders cleanly" />
      </InlineZoneErrorBoundary>,
    );
    expect(screen.queryByText(INLINE_ZONE_ERROR_FALLBACK_TEXT)).toBeNull();
    expect(screen.getByText("new zone renders cleanly")).toBeInTheDocument();
  });

  it("stays in the error state when children change but zoneId does not", () => {
    // Same zone id, same error state — the boundary does not
    // retry automatically when only the children change. The
    // caller must supply a new zoneId (or unmount the portal)
    // to retry.
    const { rerender } = render(
      <InlineZoneErrorBoundary zoneId="zone-1">
        <ThrowOnRender message="first throw" />
      </InlineZoneErrorBoundary>,
    );
    expect(screen.getByText(INLINE_ZONE_ERROR_FALLBACK_TEXT)).toBeInTheDocument();

    // New safe children with the SAME zoneId → boundary remains
    // in the error state (doesn't re-render children).
    rerender(
      <InlineZoneErrorBoundary zoneId="zone-1">
        <Safe text="would be safe if boundary reset" />
      </InlineZoneErrorBoundary>,
    );
    expect(screen.getByText(INLINE_ZONE_ERROR_FALLBACK_TEXT)).toBeInTheDocument();
    expect(screen.queryByText("would be safe if boundary reset")).toBeNull();
  });
});
