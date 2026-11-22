// The one shared answer to "is this Engram failure the project store being
// briefly unable to serve, so that the same request can succeed unchanged
// once the store is ready?". Engram identifies projection repair by code and
// a busy store by its structured readiness or control refusal.
//
// Owns: the set of transient store codes, the classifier over an
// `EngramTransportError`, and the stable API code a host route reports for
// such a refusal (`ApiErrorKind::EngramStoreTransient` in wire.rs).
// Does not own: deciding whether, how long or how often to wait. Each caller
// owns that: test-run registration's launcher waits with a bounded, printed
// backoff (scripts/test-run-registration.mjs). It also does not own reading
// the code out of Engram's output (`decode_engram_cli_json` in
// engram_host_adapter.rs).
// New module.

use super::*;

/// Engram refusal codes that mean the project store is temporarily unable to
/// serve while it repairs its derived projection. Store-open failures also
/// require the separately qualified readiness kind below.
pub(crate) const ENGRAM_STORE_TRANSIENT_CODES: &[&str] =
    &["work_projection_invalid", "projection_repair_required"];

/// The stable code a host API reports beside `error` for a refusal that
/// `engram_store_transient` classified.
pub(crate) const ENGRAM_STORE_TRANSIENT_API_CODE: &str = "engram_store_transient";

/// Whether Engram refused because its project store is temporarily unable to
/// serve, judged on structured refusal fields, never on message text. A failed CLI call
/// carries that code as `refusal_code`; a control-transport reply carries it
/// as the remote `code`.
pub(crate) fn engram_store_transient(error: &EngramTransportError) -> bool {
    let code = error.refusal_code.as_deref().or_else(|| {
        (error.kind == EngramTransportErrorKind::Remote)
            .then_some(error.code.as_deref())
            .flatten()
    });
    code.is_some_and(|code| ENGRAM_STORE_TRANSIENT_CODES.contains(&code))
        || (error.kind == EngramTransportErrorKind::Remote
            && matches!(
                error.code.as_deref(),
                Some("control_startup_busy" | "control_store_busy")
            ))
        || engram_store_readiness_open_kind(error) == Some("busy")
}

pub(crate) fn engram_store_readiness_open_kind(error: &EngramTransportError) -> Option<&str> {
    if error.refusal_code.as_deref() == Some("store_open_refused") {
        error.refusal_kind.as_deref()
    } else {
        None
    }
}
