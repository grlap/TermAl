// Which Engram evaluate refusals admission may cure with one fresh bind, and
// the assurance ladder that decision reads (Engram w-108a13d58018, tm-winf
// step 1b). Owns the pure classification: the rebind-curable refusal forms
// and whether TermAl's declared assurance covers a required level. Does not
// own the evaluate loop that acts on the classification, the bind itself, or
// the older refusal predicates that stay in `engram_host_adapter.rs`. New
// fragment beside that file, created instead of growing it.

/// Evaluate refusals that one fresh bind can cure, so admission rebinds and
/// re-evaluates once before reporting them; a second refusal after the rebind
/// is final. `stale_fence`: the claim moved on and the rebind re-reads it.
/// `control_assurance_insufficient` only in its envelope form: the refused
/// effect is one the session was bound without declaring but the current
/// `effects` include, which is what a session bound under an earlier effect
/// set (before an upgrade, or through a retained bind replayed across one)
/// meets; the rebind declares the current set. The effect must also need no
/// more assurance than the host declares, since the rebind cannot change
/// that. Its other forms, a project policy or an effect that needs more
/// assurance than the host declares, no rebind can cure, and they are
/// reported at once.
fn engram_evaluation_refusal_heals_by_rebind(
    directive: &EngramDirectiveResponse,
    effects: &[EngramEffect],
) -> bool {
    match directive.code.as_str() {
        "stale_fence" => true,
        "control_assurance_insufficient" => {
            let (Some(effect), Some(declared)) =
                (&directive.effect, &directive.declared_mediated_effects)
            else {
                return false;
            };
            // An envelope refusal names the assurance its effect needs; one
            // that names none is taken as covered, which at worst costs the
            // single bounded rebind.
            let assurance_covered = directive
                .required_assurance
                .as_deref()
                .is_none_or(engram_host_assurance_covers);
            assurance_covered
                && !declared.contains(effect)
                && effects
                    .iter()
                    .any(|current| current.wire_name() == effect.as_str())
        }
        _ => false,
    }
}

/// Whether the assurance TermAl declares (`ENGRAM_CONTROL_ASSURANCE`) covers
/// `required`. An unknown level is not covered.
fn engram_host_assurance_covers(required: &str) -> bool {
    match (
        engram_assurance_rank(ENGRAM_CONTROL_ASSURANCE),
        engram_assurance_rank(required),
    ) {
        (Some(declared), Some(required)) => required <= declared,
        _ => false,
    }
}

/// Engram's order of assurance levels: advisory, then turn_gated, then
/// action_gated.
fn engram_assurance_rank(level: &str) -> Option<u8> {
    match level {
        "advisory" => Some(0),
        "turn_gated" => Some(1),
        "action_gated" => Some(2),
        _ => None,
    }
}
