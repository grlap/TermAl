// Codex global-notice handling — the "session-wide banner" path.
//
// The shared Codex app-server occasionally sends notifications that
// are not scoped to a specific thread: config warnings, deprecation
// notices, login reminders, version updates. TermAl surfaces these
// as `CodexNotice` entries on `AppState.codex_notices`, which the UI
// renders as a dismissable banner. This file owns:
//
// - `handle_shared_codex_global_notice` — the top-level interceptor
//   called from `handle_shared_codex_app_server_notification` in
//   `codex_events.rs`. Returns `true` when the event was a recognized
//   global notice so the caller short-circuits further per-turn
//   dispatch.
// - `build_shared_codex_runtime_notice` / `build_shared_codex_global_notice`
//   — convert the raw JSON-RPC payload into a typed `CodexNotice`.
// - `infer_shared_codex_notice_level` — maps method name + payload
//   shape to the UI severity (Info / Warning / Error).
// - `extract_shared_codex_notice_text` — pulls the human-readable
//   message string out of a payload given a list of JSON pointers to
//   try.
//
// Per-turn notices (things like tool outputs or per-turn config
// rerouting) go through `push_shared_codex_turn_notice` in
// `codex_events.rs` instead.


/// Intercepts top-level `configWarning` / `deprecationNotice` events
/// that are not scoped to a thread and records them as session-wide
/// notices. Returns `true` when the event was a recognized global
/// notice so the caller can short-circuit further dispatch.
fn handle_shared_codex_global_notice(
    method: &str,
    message: &Value,
    state: &AppState,
) -> Result<bool> {
    let notice = match method {
        "configWarning" => build_shared_codex_global_notice(
            CodexNoticeKind::ConfigWarning,
            CodexNoticeLevel::Warning,
            "Config warning",
            message,
        ),
        "deprecationNotice" => build_shared_codex_global_notice(
            CodexNoticeKind::DeprecationNotice,
            CodexNoticeLevel::Info,
            "Deprecation notice",
            message,
        ),
        _ => return Ok(false),
    };

    if let Some(notice) = notice {
        state.note_codex_notice(notice)?;
    } else {
        log_unhandled_codex_event(
            &format!("failed to parse shared Codex global notice `{method}`"),
            message,
        );
    }

    Ok(true)
}

/// Builds a runtime-level notice for unknown method-bearing events that
/// lack a thread id — used as a fallback so diagnostics from Codex do
/// not get silently dropped.
fn build_shared_codex_runtime_notice(method: &str, message: &Value) -> Option<CodexNotice> {
    build_shared_codex_global_notice(
        CodexNoticeKind::RuntimeNotice,
        infer_shared_codex_notice_level(method, message),
        &format!("Codex notice: {method}"),
        message,
    )
}

/// Picks an info/warning severity for a runtime notice: prefers an
/// explicit `level`/`severity` field in the payload, otherwise infers
/// from keywords in the method name (warning/error/auth/maintenance
/// escalate to warning; everything else stays info).
fn infer_shared_codex_notice_level(method: &str, message: &Value) -> CodexNoticeLevel {
    let payload = message.get("params").unwrap_or(message);
    let severity = payload
        .get("level")
        .or_else(|| payload.get("severity"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase());

    match severity.as_deref() {
        Some("warning") | Some("warn") | Some("error") => CodexNoticeLevel::Warning,
        Some("info") | Some("notice") => CodexNoticeLevel::Info,
        _ => {
            let normalized = method.to_ascii_lowercase();
            if normalized.contains("warning")
                || normalized.contains("error")
                || normalized.contains("auth")
                || normalized.contains("maintenance")
            {
                CodexNoticeLevel::Warning
            } else {
                CodexNoticeLevel::Info
            }
        }
    }
}

/// Assembles a `CodexNotice` from a payload by probing a set of
/// pointer paths for code/title/detail. Returns `None` if none of the
/// pointers yielded enough text to form a notice worth surfacing.
fn build_shared_codex_global_notice(
    kind: CodexNoticeKind,
    level: CodexNoticeLevel,
    default_title: &str,
    message: &Value,
) -> Option<CodexNotice> {
    let payload = message.get("params").unwrap_or(message);
    let code = extract_shared_codex_notice_text(
        payload,
        &[
            "/code",
            "/id",
            "/warningCode",
            "/warning/code",
            "/deprecationId",
            "/deprecation/id",
        ],
    );
    let title = extract_shared_codex_notice_text(
        payload,
        &["/title", "/name", "/warning/title", "/deprecation/title"],
    );
    let detail = extract_shared_codex_notice_text(
        payload,
        &[
            "/detail",
            "/message",
            "/description",
            "/text",
            "/warning/message",
            "/warning/detail",
            "/deprecation/message",
            "/deprecation/detail",
        ],
    );

    let (title, detail) = match (title, detail, code.clone()) {
        (Some(title), Some(detail), _) => (title, detail),
        (Some(title), None, _) if title != default_title => (default_title.to_owned(), title),
        (None, Some(detail), _) => (default_title.to_owned(), detail),
        (None, None, Some(code)) => (default_title.to_owned(), format!("Code: `{code}`")),
        _ => return None,
    };

    Some(CodexNotice {
        kind,
        level,
        title,
        detail,
        timestamp: stamp_now(),
        code,
    })
}

/// Probes `payload` at each JSON pointer in order and returns the
/// first non-empty trimmed string found.
fn extract_shared_codex_notice_text(payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}
