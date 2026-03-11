# Feature Brief: Gemini CLI Integration

This brief tracks Gemini as a first-class TermAl agent.

Backlog source: [`docs/bugs.md`](../bugs.md)

## Problem

Only Claude and Codex are wired through session creation, runtime spawning,
message dispatch, and frontend rendering. Gemini is missing entirely.

## Likely integration shape

The most direct path is to spawn `gemini` in non-interactive mode with
`--output-format stream-json`, then map its streamed stdout events into the
same TermAl message model used for the other agents.

Reference: [`agent-integration-comparison.md`](./agent-integration-comparison.md)

## Backend tasks

- Extend `Agent` and related persistence to support Gemini.
- Add Gemini session creation and session resume plumbing.
- Implement a Gemini runtime adapter that spawns the CLI and streams events.
- Map Gemini output into TermAl message cards and turn lifecycle state.
- Decide how Gemini approval and safety behavior fits TermAl's existing
  approval model.

## Frontend tasks

- Add Gemini as a selectable agent in the UI.
- Render Gemini sessions with the same structured message types used for other
  agents where possible.
- Surface any Gemini-specific constraints or capabilities in the session UI.

## Testing

- Add adapter parsing tests for Gemini stream events.
- Add end-to-end coverage for session creation, message send, streaming, and
  restart persistence.
