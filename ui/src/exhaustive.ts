// exhaustive.ts
//
// Owns: shared exhaustive-check helper for discriminated unions and closed
// string unions.
//
// Does not own: domain-specific recovery logic, fallback behavior, or user-
// facing error copy beyond the caller-supplied assertion label.
//
// Split out of: ui/src/app-live-state.ts (`assertNever`) and
// ui/src/panels/TerminalPanel.tsx (`assertNeverTerminalOutputStream`).

export function assertNever(
  value: never,
  message = "Unexpected exhaustive value",
): never {
  throw new Error(`${message}: ${String(value)}`);
}
