// Owns the host console's stderr lines written through `eprintln!` (the
// crate-level macro in main.rs routes them here): a UTC timestamp with
// millisecond precision on every line, and per-kind rate limiting of the
// known recurring diagnostics, each reporting how many lines it suppressed
// and the last of them. It does not own stdout (the startup banner and the
// interactive prototype), check notices written to an explicit writer, or
// what any line says. A line that names an error or failure, and the first
// line of a kind in each window, are never suppressed.

/// How long a recurring kind stays quiet after it prints.
const HOST_LOG_REPEAT_WINDOW: chrono::Duration = chrono::Duration::seconds(60);

/// The recurring diagnostics, each anchored at the start of its line:
/// periodic timings, relay retries that carry only a status, and the
/// named-root reclamation pass summary and its budget deferrals. The second
/// part, when present, must follow in the same line. A diagnostic whose
/// payload is free text is not listed and is never limited: deferred wake
/// scans and executor activations, other reclamation outcomes, and every
/// ACP or Codex runtime diagnostic, whose payload is the runtime's own
/// message and can report an error in any words.
const HOST_LOG_RECURRING_MARKERS: &[(&str, Option<&str>)] = &[
    ("state lock> held ", None),
    ("state lock> waited ", None),
    ("state lock> file-change tracking ", None),
    ("native relay notification ", None),
    ("engram> named-root reclamation pass ", None),
    ("engram> named-root reclamation store=", Some("outcome=deferred: less than ")),
];

/// Words that make a line a real error, which is never suppressed. Fields
/// that report a negative flag (`commit_failed=false`, `errors=0`) are not.
const HOST_LOG_ERROR_WORDS: &[&str] = &["error", "fail", "panic", "could not", "cannot"];

const HOST_LOG_KIND_MAX_CHARS: usize = 120;
const HOST_LOG_LAST_MAX_CHARS: usize = 1_000;

/// Set once the process runs as the host server. Other modes (the
/// interactive prototype, the delegation bridge, the coordination CLI and
/// tests) write their stderr lines unchanged.
static HOST_LOG_CONSOLE: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Whether this thread is inside a host log write, holding the limiter.
    static HOST_LOG_WRITING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks this thread as writing host log lines until dropped, including
/// when the write unwinds.
struct HostLogWriting;

impl HostLogWriting {
    fn enter() -> Self {
        HOST_LOG_WRITING.with(|writing| writing.set(true));
        Self
    }
}

impl Drop for HostLogWriting {
    fn drop(&mut self) {
        HOST_LOG_WRITING.with(|writing| writing.set(false));
    }
}

fn host_log_line(args: std::fmt::Arguments<'_>) {
    static LIMITER: LazyLock<Mutex<HostLogLimiter>> =
        LazyLock::new(|| Mutex::new(HostLogLimiter::default()));
    // Both paths write through std's own macro, which keeps its semantics,
    // including the output capture of the test harness.
    if !HOST_LOG_CONSOLE.load(Ordering::Relaxed) {
        ::std::eprintln!("{args}");
        return;
    }
    let line = args.to_string();
    // A line logged while this thread already writes host log lines (a panic
    // hook that logs while a write panics) bypasses the limiter, which this
    // thread holds: it is stamped and written directly, never waited on.
    if HOST_LOG_WRITING.with(std::cell::Cell::get) {
        ::std::eprintln!("{}", host_log_stamped(&host_log_timestamp(chrono::Utc::now()), &line));
        return;
    }
    let _writing = HostLogWriting::enter();
    // Stamped and written under the limiter, so console order follows
    // timestamp order.
    let mut limiter = LIMITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = chrono::Utc::now();
    for line in limiter.admit(&line, now) {
        ::std::eprintln!("{line}");
    }
}

/// Whether `line` reports a real error, ignoring fields that report a
/// negative flag or a zero count, such as `commit_failed=false`.
fn host_log_names_error(line: &str) -> bool {
    let lower = line.to_lowercase();
    let reported = lower
        .split_whitespace()
        .filter(|token| !(token.ends_with("=false") || token.ends_with("=0")))
        .collect::<Vec<_>>()
        .join(" ");
    HOST_LOG_ERROR_WORDS.iter().any(|word| reported.contains(word))
}

/// `line` with the timestamp at the start of each of its physical lines.
fn host_log_stamped(stamp: &str, line: &str) -> String {
    line.split('\n').map(|part| format!("{stamp} {part}")).collect::<Vec<_>>().join("\n")
}

fn host_log_timestamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The recurring kind of `line`, or `None` when it is never rate limited:
/// lines without a recurring marker, and lines that name an error.
fn host_log_recurring_kind(line: &str) -> Option<String> {
    if host_log_names_error(line) {
        return None;
    }
    let end = HOST_LOG_RECURRING_MARKERS.iter().find_map(|(prefix, then)| {
        if !line.starts_with(prefix) {
            return None;
        }
        match then {
            None => Some(prefix.len()),
            Some(then) => line.find(then).map(|at| at + then.len()),
        }
    })?;
    // The kind is the anchored marker and the words after it, up to the
    // first number, colon or parenthesis, so ids, timings and payloads
    // share one kind.
    let rest = &line[end..];
    let stable = rest
        .find(|c: char| c.is_ascii_digit() || c == ':' || c == '(')
        .map_or(rest, |cut| &rest[..cut]);
    Some(
        format!("{}{}", &line[..end], stable)
            .trim_end()
            .chars()
            .take(HOST_LOG_KIND_MAX_CHARS)
            .collect(),
    )
}

#[derive(Debug)]
struct HostLogKindWindow {
    printed_at: chrono::DateTime<chrono::Utc>,
    suppressed: u64,
    last: String,
}

#[derive(Debug, Default)]
struct HostLogLimiter {
    kinds: HashMap<String, HostLogKindWindow>,
}

impl HostLogLimiter {
    /// The timestamped console lines to write for `line` at `now`: the line
    /// itself unless its kind printed within the window, preceded by a
    /// summary for every kind whose window closed with suppressed lines.
    fn admit(&mut self, line: &str, now: chrono::DateTime<chrono::Utc>) -> Vec<String> {
        let stamp = host_log_timestamp(now);
        let mut out = self.flush_expired(now, &stamp);
        let Some(kind) = host_log_recurring_kind(line) else {
            out.push(host_log_stamped(&stamp, line));
            return out;
        };
        match self.kinds.get_mut(&kind) {
            Some(window) if now - window.printed_at < HOST_LOG_REPEAT_WINDOW => {
                window.suppressed += 1;
                window.last = line.chars().take(HOST_LOG_LAST_MAX_CHARS).collect();
            }
            _ => {
                self.kinds.insert(kind, HostLogKindWindow {
                    printed_at: now, suppressed: 0, last: String::new(),
                });
                out.push(host_log_stamped(&stamp, line));
            }
        }
        out
    }

    /// One summary line per kind whose window has closed with suppressed
    /// lines; the kind then prints its next line as a first occurrence.
    fn flush_expired(&mut self, now: chrono::DateTime<chrono::Utc>, stamp: &str) -> Vec<String> {
        let mut expired: Vec<_> = self
            .kinds
            .iter()
            .filter(|(_, window)| now - window.printed_at >= HOST_LOG_REPEAT_WINDOW)
            .map(|(kind, _)| kind.clone())
            .collect();
        expired.sort();
        expired
            .into_iter()
            .filter_map(|kind| {
                let window = self.kinds.remove(&kind)?;
                (window.suppressed > 0).then(|| {
                    host_log_stamped(stamp, &format!(
                        "{kind}: {} similar line(s) suppressed since {}; last: {}",
                        window.suppressed,
                        host_log_timestamp(window.printed_at),
                        window.last,
                    ))
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod host_log_tests {
    use super::*;

    fn at(millis: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-11-03T03:52:00.000Z").unwrap().to_utc()
            + chrono::Duration::milliseconds(millis)
    }

    /// The file-change timing line exactly as the state-lock reporter prints it.
    fn file_change_line(commit_failed: bool) -> String {
        let mut diagnostic = FileChangeTrackingDiagnostic::default();
        diagnostic.commit_attempted = true;
        diagnostic.commit_failed = commit_failed;
        format!("state lock> file-change tracking {diagnostic}")
    }

    #[test]
    fn every_physical_line_carries_a_utc_millisecond_timestamp() {
        let mut limiter = HostLogLimiter::default();
        assert_eq!(limiter.admit("startup> listener bound elapsed_ms=12", at(728)),
            ["2026-11-03T03:52:00.728Z startup> listener bound elapsed_ms=12"]);
        assert_eq!(limiter.admit("engram> failed:\ncaused by: disk full", at(1_005)),
            ["2026-11-03T03:52:01.005Z engram> failed:\n2026-11-03T03:52:01.005Z caused by: disk full"]);
    }

    #[test]
    fn ordinary_lines_errors_and_error_carrying_deferrals_are_never_suppressed() {
        let mut limiter = HostLogLimiter::default();
        let lines = [
            "engram> observation worker kick failed: busy".to_owned(),
            "state lock> failed to start diagnostic reporter: denied".to_owned(),
            "engram> named-root reclamation could not start: offline".to_owned(),
            "codex diagnostic> unhandled shared Codex app-server notification `x`: error payload".to_owned(),
            "codex diagnostic> unhandled shared Codex app-server notification `thread/tokenUsage`: {}".to_owned(),
            r#"gemini acp diagnostic> unhandled ACP notification `log`: {"level":"fatal","message":"database is locked"}"#.to_owned(),
            "native relay notification 18 deferred (500 Internal Server Error)".to_owned(),
            "native mailbox wake scan deferred: database is locked".to_owned(),
            "native mailbox wake scan deferred: database disk image is malformed".to_owned(),
            "received executor activation deferred (503): store unavailable".to_owned(),
            "engram> named-root reclamation store=s work=w-1 generation=2 route=Live outcome=retained: database is locked".to_owned(),
            file_change_line(true),
            "mailbox> delivered 3 messages".to_owned(),
        ];
        for line in &lines {
            for repeat in 0..20 {
                assert_eq!(limiter.admit(line, at(repeat * 100)).len(), 1, "{line} is never suppressed");
            }
        }
    }

    #[test]
    fn negative_flags_do_not_make_a_line_an_error() {
        assert!(!host_log_names_error(&file_change_line(false)), "commit_failed=false is not a failure");
        assert!(host_log_names_error(&file_change_line(true)), "commit_failed=true is a failure");
        assert!(!host_log_names_error("relay> retried errors=0"));
        assert!(host_log_names_error("relay> retried errors=3"));
    }

    #[test]
    fn recurring_kinds_ignore_numbers_and_payloads_but_keep_their_words() {
        let kind = |line: &str| host_log_recurring_kind(line).unwrap();
        assert_eq!(kind("state lock> held 12ms by src/a.rs:1:2 (acquisition wait 0ms)"), "state lock> held");
        assert_eq!(kind("state lock> held 340ms by src/b.rs:9:9 (acquisition wait 3ms)"), "state lock> held");
        assert_eq!(kind("state lock> waited 40ms at src/a.rs:1:2"), "state lock> waited");
        assert_eq!(kind(&file_change_line(false)), "state lock> file-change tracking preparation_ms=");
        assert_eq!(kind("native relay notification 17 deferred (409 Conflict)"), "native relay notification");
        assert_eq!(kind("native relay notification 91 deferred (409 Conflict)"), "native relay notification");
        assert_eq!(kind("engram> named-root reclamation pass attempted=3 limit=8 elapsed_ms=12"),
            "engram> named-root reclamation pass attempted=");
        for unlisted in ["engram> no source basis for the named root",
            "codex diagnostic> unhandled shared Codex app-server notification `a/b`: {}",
            "claude acp diagnostic> unhandled ACP notification `x`: {}",
            r#"gemini acp diagnostic> unhandled ACP notification `log`: {"level":"fatal","message":"database is locked"}"#,
            "codex diagnostic> Codex rate limit notification missing params.rateLimits",
            "claude acp diagnostic> ACP message missing method",
            "api> a named-root reclamation pass or another release is still running",
            "mailbox storage is temporarily busy while retaining native relay notification 5",
            "relay> state lock> held 12ms by src/a.rs:1:2",
            "engram> named-root reclamation store=s work=w-1 generation=2 route=Live outcome=retained: database is locked",
            "engram> named-root reclamation store=s work=w-1 generation=2 route=Live outcome=retired",
            "native mailbox wake scan deferred: busy", "received executor activation deferred (409): busy"] {
            assert_eq!(host_log_recurring_kind(unlisted), None, "{unlisted}");
        }
    }

    #[test]
    fn a_recurring_flood_drops_from_ten_lines_a_second_to_one_line_per_kind_a_minute() {
        // The reported flood: about ten recurring diagnostic lines a second,
        // in the formats the host prints.
        let flood = [
            "state lock> held 120ms by src/state.rs:10:4 (acquisition wait 0ms)".to_owned(),
            file_change_line(false),
            "native relay notification 17 deferred (409 Conflict)".to_owned(),
            "state lock> waited 40ms at src/state.rs:10:4".to_owned(),
            "engram> named-root reclamation store=fixture work=w-1 generation=2 route=Live outcome=deferred: less than 50 ms of the pass budget left".to_owned(),
        ];
        let mut limiter = HostLogLimiter::default();
        let mut before = 0;
        let mut after = Vec::new();
        for tick in 0..600 {
            before += 1;
            after.extend(limiter.admit(&flood[tick % flood.len()], at(tick as i64 * 100)));
        }
        assert_eq!(before, 600, "60 s at ten lines a second");
        assert_eq!(after.len(), flood.len(), "one first occurrence per kind in the minute: {after:#?}");

        // Once every window has closed, the next line brings one summary per
        // kind, each with its exact suppressed count and last line.
        let next = limiter.admit(&flood[0], at(60_400));
        assert_eq!(next.len(), flood.len() + 1, "{next:#?}");
        for (summary, kind) in next.iter().zip([
            "engram> named-root reclamation store=fixture work=w-1 generation=2 route=Live outcome=deferred: less than",
            "native relay notification",
            "state lock> file-change tracking preparation_ms=",
            "state lock> held",
            "state lock> waited",
        ]) {
            assert!(summary.starts_with(&format!("2026-11-03T03:53:00.400Z {kind}: 119 similar line(s) suppressed since ")),
                "{summary}");
            let last = flood.iter().find(|line| host_log_recurring_kind(line).unwrap() == kind).unwrap();
            assert!(summary.ends_with(&format!("; last: {last}")), "{summary}");
        }
        assert_eq!(next.last().unwrap(), &format!("2026-11-03T03:53:00.400Z {}", flood[0]));
    }

    #[test]
    fn a_summary_stamps_every_physical_line_of_its_last_message() {
        let mut limiter = HostLogLimiter::default();
        limiter.admit("state lock> held 120ms by src/a.rs:1:2", at(0));
        limiter.admit("state lock> held 900ms by src/b.rs:3:4
continued detail", at(100));
        let next = limiter.admit("mailbox> delivered", at(60_000));
        assert_eq!(next, [
            "2026-11-03T03:53:00.000Z state lock> held: 1 similar line(s) suppressed since 2026-11-03T03:52:00.000Z; last: state lock> held 900ms by src/b.rs:3:4
2026-11-03T03:53:00.000Z continued detail",
            "2026-11-03T03:53:00.000Z mailbox> delivered",
        ]);
    }

    #[test]
    fn the_reentry_guard_clears_when_a_write_unwinds() {
        assert!(!HOST_LOG_WRITING.with(std::cell::Cell::get));
        let unwound = std::panic::catch_unwind(|| {
            let _writing = HostLogWriting::enter();
            assert!(HOST_LOG_WRITING.with(std::cell::Cell::get), "a nested log on this thread bypasses the limiter");
            panic!("the stderr write failed");
        });
        assert!(unwound.is_err());
        assert!(!HOST_LOG_WRITING.with(std::cell::Cell::get), "the thread logs through the limiter again");
    }
}
