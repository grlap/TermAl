// The one shared answer to "is this Engram failure the project store being
// briefly unable to serve, so that the same request can succeed unchanged
// once the store is ready?". Today that is a store under projection repair,
// which Engram reports with a typed refusal code.
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

/// Engram refusal codes that mean the project store is temporarily unable to
/// serve while it repairs its derived projection. Any other code, and any
/// failure without an Engram code, is not transient here.
const ENGRAM_STORE_TRANSIENT_CODES: &[&str] =
    &["work_projection_invalid", "projection_repair_required"];

/// The stable code a host API reports beside `error` for a refusal that
/// `engram_store_transient` classified.
const ENGRAM_STORE_TRANSIENT_API_CODE: &str = "engram_store_transient";

/// Whether Engram refused because its project store is in repair, judged on
/// Engram's own refusal code alone, never on message text. A failed CLI call
/// carries that code as `refusal_code`; a control-transport reply carries it
/// as the remote `code`.
fn engram_store_transient(error: &EngramTransportError) -> bool {
    let code = error.refusal_code.as_deref().or_else(|| {
        (error.kind == EngramTransportErrorKind::Remote)
            .then_some(error.code.as_deref())
            .flatten()
    });
    code.is_some_and(|code| ENGRAM_STORE_TRANSIENT_CODES.contains(&code))
}
