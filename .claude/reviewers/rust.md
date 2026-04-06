# Rust Review

Focus: Error handling, concurrency, async safety, idiomatic Rust patterns.
## Development-Phase Compatibility Policy
- Legacy compatibility means supporting older persisted schema or older local/internal API shapes from previous development builds, such as obsolete orchestrator fields.
- Do NOT flag missing schema upgrades, migrations, or backward compatibility for ~/.termal/*.json, browser localStorage state, or local/internal API contracts from previous local-only development builds.
- Path normalization and canonicalization for current inputs are not legacy compatibility work.
- Intentional breaking changes are acceptable during development; only flag compatibility issues when they break current-tree behavior, current tests, or the current documented contract.
- Windows, macOS, and Linux are P0 platforms. Flag regressions on those platforms; do not require support beyond them unless the current change claims it.

## What to check

1. **Error handling**: Proper use of `Result` and error propagation:
   - Flag `.unwrap()` on `Result` or `Option` in non-test code without justification
   - Flag `panic!()` or `unreachable!()` in request handlers (should return `ApiError`)
   - Flag bare `_` in error match arms that silently discard errors — at minimum log them
   - Prefer `?` operator over manual match-and-return for error propagation
   - `ApiError` variants should use appropriate HTTP status codes (400 vs 404 vs 500)

2. **Async safety with tokio**:
   - Flag `std::sync::Mutex` held across `.await` points (use `tokio::sync::Mutex` or restructure)
   - Flag blocking operations (`std::fs`, `std::thread::sleep`) inside async tasks (use `tokio::fs`, `tokio::time::sleep`)
   - Flag spawning threads with `std::thread::spawn` where `tokio::spawn` would work
   - Exception: the 4-thread-per-runtime pattern (writer, reader, stderr, waiter) intentionally uses `std::thread` for stdio — this is correct

3. **JSON serialization**: Correct use of serde:
   - Flag missing `#[serde(rename_all = "camelCase")]` on structs sent to the frontend
   - Flag `#[serde(skip)]` on fields that should be persisted
   - Flag `#[serde(default)]` missing only when the current tree intentionally allows field absence on live read/write paths and the current contract depends on graceful handling
   - Flag manual JSON construction (`json!({...})`) where a typed struct with `serde::Serialize` would be safer

4. **String handling**:
   - Flag `.clone()` on `String` where `&str` would suffice
   - Flag repeated `.to_owned()` / `.to_string()` in hot paths
   - Flag `format!()` for simple concatenation where `push_str()` or string interpolation would be cleaner

5. **Match exhaustiveness**: When adding new enum variants:
   - All `match` arms must handle the new variant (or use a wildcard with a comment explaining why)
   - Flag `_ =>` catch-all arms that could hide missing handling for new variants
   - Especially important for `SessionStatus`, `Message` type routing, and agent type dispatch

6. **Resource cleanup**:
   - Flag child processes spawned without corresponding cleanup on error/shutdown
   - Flag channels (`mpsc`, `broadcast`) created without proper drop handling
   - Flag file handles or temp files not cleaned up on error paths

7. **Thread safety**:
   - Flag `Rc` or `RefCell` in code shared across threads (must use `Arc`/`Mutex`)
   - Flag `Send`/`Sync` bound violations
   - Flag shared mutable state without synchronization

## What NOT to flag

- Using `std::thread` for agent runtime threads (intentional — these do blocking stdio)
- Using `std::sync::Mutex` for `AppState.inner` (held briefly, never across await)
- Single-file architecture (`main.rs`) — known tradeoff
- `clone()` on small types like `String` session IDs in non-hot paths
- `expect("state mutex poisoned")` on mutex locks — this is the project pattern

