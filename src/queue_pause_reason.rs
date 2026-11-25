// Owns the typed reason a session's queue was paused for, and the bounded
// failure kind a failed turn carries into it. Each pause site names its own
// cause; the record keeps the latest reason until the pause is lifted.
// Does not decide when to pause or resume, and never stores provider message
// text: the failure kind comes only from typed provider error signals.
// New module; the pause writer it serves is SessionRecord::pause_auto_dispatch
// in state.rs.

use super::*;

/// Why a session's queue is paused. Unknown values written by a newer build
/// decode as `Unknown` rather than failing the whole record.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueuePauseCause {
    /// A mailbox-woken turn failed; the restored wake is held so the same
    /// queue head is not retried before someone looks at the failure.
    TurnFailed,
    /// The runtime exited during a mailbox-woken turn.
    RuntimeExited,
    /// The provider reported it lacks capacity; recovery retries by itself.
    ProviderCapacity,
    /// A Stop paused the queue.
    StopRequested,
    /// A Codex terminal could not be matched to its turn.
    TerminalUnresolved,
    /// A source-observation removal holds the queue until an explicit Resume.
    SourceObservationHold,
    /// An accepted human prompt failed to start.
    HumanStartFailed,
    /// A dedicated runtime reset failed.
    RuntimeResetFailed,
    /// The exited runtime's cleanup failed; Stop or Delete retries it.
    RuntimeCleanupFailed,
    /// Saving the session's state failed after a turn ended.
    PersistFailed,
    #[serde(other)]
    Unknown,
}

/// The bounded kind of a failed turn, derived only from typed provider
/// signals. Message text is never classified.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnFailureKind {
    ProviderUsageLimit,
    ProviderAuth,
    ProviderOverloaded,
    ProviderOther,
    RuntimeCrashed,
    #[serde(other)]
    Unclassified,
}

impl TurnFailureKind {
    /// Classifies Claude's numeric API error status.
    pub(crate) fn from_claude_api_status(status: u64) -> Self {
        match status {
            429 => Self::ProviderUsageLimit,
            401 | 403 => Self::ProviderAuth,
            503 | 529 => Self::ProviderOverloaded,
            _ => Self::ProviderOther,
        }
    }

    /// Classifies Codex's typed `codexErrorInfo` code.
    pub(crate) fn from_codex_error_info(code: &str) -> Self {
        match code {
            "usageLimitExceeded" => Self::ProviderUsageLimit,
            "unauthorized" => Self::ProviderAuth,
            "serverOverloaded" => Self::ProviderOverloaded,
            _ => Self::ProviderOther,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueuePauseReason {
    pub(crate) cause: QueuePauseCause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure_kind: Option<TurnFailureKind>,
    /// A failed turn's one-line summary: the leading plain-prose words of the
    /// first line of its failure, bounded to `PAUSE_SUMMARY_MAX_CHARS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<String>,
    pub(crate) paused_at: String,
}

const PAUSE_SUMMARY_MAX_CHARS: usize = 120;

const PAUSE_SUMMARY_REDACTED: &str = "[redacted]";
const PAUSE_SUMMARY_ELLIPSIS: &str = "...";

/// The longest word kept as plain prose; a longer run of letters may be an
/// opaque credential.
const PAUSE_SUMMARY_MAX_WORD_CHARS: usize = 32;

/// A failed turn's summary: the leading words of the first line of its
/// failure for as long as each is plain prose, then `[redacted]` in place of
/// the rest of the line, bounded to `PAUSE_SUMMARY_MAX_CHARS` characters
/// including any truncation marker.
///
/// A plain word is ASCII letters, with `'` or `-` only between letters, or a
/// number of at most three digits such as an HTTP status; it may open with
/// `(` and end with up to two of `. , ; : ! ? )`, and holds at most
/// `PAUSE_SUMMARY_MAX_WORD_CHARS` characters, punctuation included. Once any
/// part of a plain word (split at `'`, `-`, camelCase and acronym
/// boundaries), or any segment between `'` and `-` with case ignored, is a
/// credential label (`key`, `tokens`, `Authorization`, `Bearer` and the like,
/// singular or plural), or starts with one of the longer labels
/// (`APITokenabc`, `TOKENhunter`), the word is kept through the earliest such
/// label and any trailing punctuation and ends the summary; anything joined
/// to the label in the same word, as in `password-hunter` or
/// `passwordHunter`, is its value. A short label glued to lowercase letters
/// inside one part (`pwdhunter`) is not recognised: the stated residual.
/// A word that is not plain, or that starts with a known token prefix such as
/// `sk-` or `ghp_`, ends it before itself. Only this allowlist is kept, so
/// URLs, JSON, `key=value` text and tokens never reach the summary. Best
/// effort: a credential made only of letters, or of at most three digits,
/// that follows no label is not recognised.
///
/// Only the first non-empty line counts: within a line only spaces and tabs
/// separate words, and any other whitespace (a line break, a lone CR, a
/// vertical tab or form feed, the Unicode line separators) ends it. At most
/// one character past the bound is read from it, so the work is linear and
/// bounded whatever the failure's size. The kept text is a prefix of that
/// line, and a word cut by the read limit is not kept, so the bound never
/// shows part of a word that was not judged whole; when the rest is
/// redacted, the kept words give way so that `[redacted]` is shown whole.
/// Returns None for an empty failure.
pub(crate) fn queue_pause_summary(failure: &str) -> Option<String> {
    // The first non-empty line, read only as far as the bound needs: leading
    // whitespace and blank lines are skipped (every line break is whitespace),
    // and characters past one beyond the bound are cut by it anyway.
    let mut head = String::new();
    let mut head_chars = 0;
    let mut line_continues = false;
    for ch in failure.chars().skip_while(|ch| ch.is_whitespace()) {
        // Within a line only spaces and tabs separate words; any other
        // whitespace (a line break, a vertical tab or form feed, the Unicode
        // line separators) ends the line.
        if ch.is_whitespace() && !matches!(ch, ' ' | '\t') {
            break;
        }
        if head_chars > PAUSE_SUMMARY_MAX_CHARS {
            line_continues = true;
            break;
        }
        head.push(ch);
        head_chars += 1;
    }
    if head.is_empty() {
        return None;
    }
    let words = summary_words(&head);
    let mut kept_end = 0;
    let mut redact_rest = false;
    for (start, word) in words {
        let end = start + word.len();
        // A word the read limit cut is not judged on its visible part.
        let cut_by_limit = line_continues && end == head.len();
        match classify_summary_word(word) {
            SummaryWord::Plain if !cut_by_limit => kept_end = end,
            SummaryWord::Label { kept } if !cut_by_limit => {
                // The word is kept through its label only; anything joined
                // to the label in the same word is its value.
                kept_end = start + kept;
                redact_rest = kept < word.len() || line_continues || !head[end..].trim().is_empty();
                break;
            }
            _ => {
                redact_rest = true;
                break;
            }
        }
    }
    let kept = &head[..kept_end];
    if !redact_rest {
        return Some(bounded_summary(kept, PAUSE_SUMMARY_MAX_CHARS));
    }
    // The marker always fits whole: the kept words give way to it.
    if kept.is_empty() {
        return Some(PAUSE_SUMMARY_REDACTED.to_owned());
    }
    let room = PAUSE_SUMMARY_MAX_CHARS - PAUSE_SUMMARY_REDACTED.len() - 1;
    Some(format!(
        "{} {PAUSE_SUMMARY_REDACTED}",
        bounded_summary(kept, room)
    ))
}

/// Text cut to at most `limit` characters, ending in the truncation marker
/// when it was cut.
fn bounded_summary(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut bounded = text
        .chars()
        .take(limit - PAUSE_SUMMARY_ELLIPSIS.len())
        .collect::<String>();
    bounded.push_str(PAUSE_SUMMARY_ELLIPSIS);
    bounded
}

/// The whitespace-separated words of a text, each with its byte offset.
fn summary_words(text: &str) -> Vec<(usize, &str)> {
    let mut words = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(begin) = start.take() {
                words.push((begin, &text[begin..index]));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        words.push((begin, &text[begin..]));
    }
    words
}

#[derive(Debug, PartialEq, Eq)]
enum SummaryWord {
    /// Plain prose, kept.
    Plain,
    /// A plain word that names a credential: its first `kept` bytes, through
    /// the label and any trailing punctuation, are kept, and it ends the
    /// summary.
    Label { kept: usize },
    /// Anything else: ends the summary before itself.
    Other,
}

fn classify_summary_word(word: &str) -> SummaryWord {
    // The whole word, punctuation included, is what would be shown.
    if word.len() > PAUSE_SUMMARY_MAX_WORD_CHARS {
        return SummaryWord::Other;
    }
    let core = word.strip_prefix('(').unwrap_or(word);
    let lead = word.len() - core.len();
    let mut core = core;
    for _ in 0..2 {
        core = core
            .strip_suffix(['.', ',', ';', ':', '!', '?', ')'])
            .unwrap_or(core);
    }
    if core.is_empty() || core.len() > PAUSE_SUMMARY_MAX_WORD_CHARS {
        return SummaryWord::Other;
    }
    let bytes = core.as_bytes();
    if bytes.iter().all(u8::is_ascii_digit) {
        return if bytes.len() <= 3 {
            SummaryWord::Plain
        } else {
            SummaryWord::Other
        };
    }
    let letters_only = bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.last().is_some_and(u8::is_ascii_alphabetic)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'\'' | b'-'))
        && !bytes
            .windows(2)
            .any(|pair| !pair[0].is_ascii_alphabetic() && !pair[1].is_ascii_alphabetic());
    if !letters_only || has_token_prefix(core) {
        return SummaryWord::Other;
    }
    // Once any part of the word is, or starts with, a label, the word is
    // kept through the earliest such label and the rest of it is redacted.
    // Parts are read two ways, so that a case change inside a label cannot
    // hide it: split at case and acronym changes as well (`myAPIToken`), and
    // split only at `'` and `-` with case ignored (`APIkey`, `TOKENhunter`).
    let label_end = label_parts(core)
        .into_iter()
        .chain(label_segments(core))
        .filter_map(|(part, end)| {
            if is_credential_label_part(&part) {
                return Some(end);
            }
            credential_label_prefix_len(&part).map(|len| end - part.len() + len)
        })
        .min();
    match label_end {
        // A label that ends the word keeps its trailing punctuation; one
        // followed by more of the word, as in `password-hunter`, is kept
        // only through the label.
        Some(end) if end == core.len() => SummaryWord::Label { kept: word.len() },
        Some(end) => SummaryWord::Label { kept: lead + end },
        None => SummaryWord::Plain,
    }
}

/// The lowercased segments of an ASCII word between `'` and `-`, each with
/// the offset just past it, so `APIkey` is the one segment `apikey`.
fn label_segments(word: &str) -> Vec<(String, usize)> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (index, ch) in word.char_indices() {
        if matches!(ch, '\'' | '-') {
            if index > start {
                segments.push((word[start..index].to_ascii_lowercase(), index));
            }
            start = index + 1;
        }
    }
    if word.len() > start {
        segments.push((word[start..].to_ascii_lowercase(), word.len()));
    }
    segments
}

/// The length of the credential label a word part starts with, when the
/// rest of the part is glued to it (`Tokenabc` in `APITokenabc`). Only the
/// longer labels count here, so ordinary words such as `keyboard`,
/// `authentication` or `basically` are not read as a label and its value.
fn credential_label_prefix_len(part: &str) -> Option<usize> {
    [
        "authorization",
        "credential",
        "signature",
        "password",
        "passwd",
        "apikey",
        "secret",
        "cookie",
        "token",
    ]
    .into_iter()
    .find(|label| part.len() > label.len() && part.starts_with(label))
    .map(str::len)
}

/// Prefixes of well-known token formats.
fn has_token_prefix(word: &str) -> bool {
    ["sk-", "sk_", "ghp_", "gho_", "xox", "AKIA", "eyJ"]
        .iter()
        .any(|prefix| word.starts_with(prefix))
}

/// The lowercased parts of an ASCII word, each with the offset just past it:
/// split at `'` and `-`, where a lowercase letter meets an uppercase one, and
/// before the last capital of an acronym that a lowercase letter follows, so
/// `accessToken` is `access` and `token`, and `myAPIToken` is `my`, `api` and
/// `token`.
fn label_parts(word: &str) -> Vec<(String, usize)> {
    let chars = word.chars().collect::<Vec<_>>();
    let mut parts = Vec::new();
    let mut part = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        if !ch.is_ascii_alphabetic() {
            if !part.is_empty() {
                parts.push((std::mem::take(&mut part), index));
            }
            continue;
        }
        let previous = index.checked_sub(1).map(|before| chars[before]);
        let next = chars.get(index + 1).copied();
        let starts_word = ch.is_ascii_uppercase()
            && previous.is_some_and(|before| {
                before.is_ascii_lowercase()
                    || (before.is_ascii_uppercase()
                        && next.is_some_and(|after| after.is_ascii_lowercase()))
            });
        if starts_word && !part.is_empty() {
            parts.push((std::mem::take(&mut part), index));
        }
        part.push(ch.to_ascii_lowercase());
    }
    if !part.is_empty() {
        parts.push((part, chars.len()));
    }
    parts
}

/// A credential label, singular or with a plural `s`.
fn is_credential_label_part(part: &str) -> bool {
    let is_label = |name: &str| {
        matches!(
            name,
            "key"
                | "apikey"
                | "token"
                | "secret"
                | "password"
                | "passwd"
                | "pwd"
                | "authorization"
                | "auth"
                | "bearer"
                | "basic"
                | "digest"
                | "credential"
                | "cookie"
                | "sig"
                | "signature"
                | "jwt"
                | "otp"
        )
    };
    is_label(part) || part.strip_suffix('s').is_some_and(is_label)
}

impl QueuePauseReason {
    pub(crate) fn now(cause: QueuePauseCause) -> Self {
        Self {
            cause,
            failure_kind: None,
            summary: None,
            paused_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }
    }

    /// A failed or exited turn, with the typed kind its provider reported for
    /// that turn, or `Unclassified` when it reported none, and the failure's
    /// summary from `queue_pause_summary`, computed before the state lock.
    pub(crate) fn turn(
        cause: QueuePauseCause,
        kind: Option<TurnFailureKind>,
        summary: Option<String>,
    ) -> Self {
        Self {
            failure_kind: Some(kind.unwrap_or(TurnFailureKind::Unclassified)),
            summary,
            ..Self::now(cause)
        }
    }
}

impl AppState {
    /// Records the typed kind a provider reported for the session's current
    /// turn, so the pause that turn's failure causes can name it. Called only
    /// from typed provider signals, never from rendered message text.
    pub(crate) fn note_turn_failure_kind(
        &self,
        session_id: &str,
        runtime: &RuntimeToken,
        generation: u64,
        kind: TurnFailureKind,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        // Recorded during a Stop too: a failed Stop replays the turn's
        // deferred terminal, whose pause then needs this kind. Either way the
        // kind describes this turn only; the next turn clears it.
        if record.runtime.matches_runtime_token(runtime)
            && record.active_turn_generation == generation
        {
            record.turn_failure_kind = Some(kind);
        }
    }

    /// The reason a local session's queue is paused now, if it is paused and
    /// the pause recorded one. A remote proxy's reason is its host's.
    pub(crate) fn local_queue_pause_reason(&self, session_id: &str) -> Option<QueuePauseReason> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(session_id)?];
        (record.is_local_session() && record.orchestrator_auto_dispatch_blocked)
            .then(|| record.queue_pause_reason.clone())
            .flatten()
    }

    /// The pause reason of the local session whose current mailbox binding
    /// is this domain principal, if it is paused and the pause recorded one.
    /// A principal bound to another host matches no local session.
    pub(crate) fn local_queue_pause_reason_for_participant(
        &self,
        participant_key: &str,
    ) -> Option<QueuePauseReason> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .sessions
            .iter()
            .filter(|record| record.is_local_session() && record.orchestrator_auto_dispatch_blocked)
            .find(|record| {
                record
                    .mailbox_binding()
                    .is_some_and(|binding| binding.participant_key() == participant_key)
            })
            .and_then(|record| record.queue_pause_reason.clone())
    }
}
