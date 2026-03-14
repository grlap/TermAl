# Security Review

Focus: Process spawning, file system access, input validation, local attack surface.

## What to check

1. **Child process spawning**: Agent runtimes are spawned as child processes:
   - Flag command arguments constructed from user input without sanitization
   - Flag environment variables leaked to child processes that shouldn't have them
   - Flag missing process cleanup on error paths (zombie processes)
   - Verify `CLAUDE_CODE_ENTRYPOINT=termal` is the only custom env var set
   - Flag shell injection vectors (user text passed to shell commands)

2. **File system access**: The `/api/file` and `/api/fs` endpoints expose file operations:
   - Flag path traversal vulnerabilities (`../` in file paths not normalized)
   - Flag reading/writing files outside the session's workdir without validation
   - Flag symlink following that could escape the intended directory
   - Flag missing file size limits on read/write operations

3. **Input validation on API endpoints**:
   - Flag missing validation on `POST /api/sessions/{id}/messages` (empty text, oversized payloads)
   - Flag missing session ID validation (format, existence)
   - Flag approval decisions accepted for non-pending approvals
   - Flag missing Content-Type validation on request bodies

4. **SSE event stream**:
   - Flag session data leaked to unauthorized clients (currently single-user, but verify no cross-session leaks in multi-tab scenarios)
   - Flag sensitive data (file contents, credentials) included in SSE broadcast when only metadata is needed

5. **Image attachment handling**:
   - Flag base64 data not validated for size limits (memory exhaustion)
   - Flag media type not validated against allowlist
   - Flag base64 data stored without sanitization (XSS via data URIs)

6. **CORS and network exposure**:
   - The server binds to `0.0.0.0:8787` — flag if CORS headers allow any origin
   - Flag missing rate limiting on expensive endpoints (session creation, file reads)
   - Flag sensitive information in error responses (stack traces, internal paths)

7. **Secrets in persistence**:
   - Flag API keys, tokens, or credentials stored in `~/.termal/sessions.json`
   - Flag session messages that might contain user secrets persisted in plain text
   - Flag agent output containing credentials not redacted before persistence

## What NOT to flag

- Single-user local-only deployment model (Phase 1 design — no auth needed)
- Claude/Codex having access to the full filesystem (they are local tools run by the user)
- `0.0.0.0` binding (configurable via `TERMAL_PORT`, documented as local-only)
- Message history containing file contents (expected behavior for a code agent UI)
