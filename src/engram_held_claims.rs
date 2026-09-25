// The one-shot CLI read of the work binding a session passes to
// `session_bind`, through `engram work core held`, and the choice among the
// claims it lists (Engram w-108a13d58018 criterion 3, tm-winf step 3). Owns
// the reader, the shape of the held-claims output TermAl reads, and the
// selection of one binding from it. Does not own when the binding is read
// (the bind in `engram_host_adapter.rs`, the queued bind preparation in
// `engram_queued_admission.rs`, the admission refresh in
// `engram_work_binding_refresh.rs`) or the CLI process plumbing it calls
// (`run_engram_json_command_with_lock_retry`, which stays in
// `engram_host_adapter.rs`). `read_engram_work_binding_from_cli` moved here
// from `engram_host_adapter.rs` when it changed from the focus-based read
// (`work core next --sections focus`, then `work core focus`) to this one.

/// Names the caller in every failure message of a one-shot Engram CLI call.
const ENGRAM_WORK_BINDING_READER_LABEL: &str = "work-binding reader";

/// What an Engram build without `work core held` prints on stderr when asked
/// for it (its argument parser's refusal).
const ENGRAM_WORK_CORE_HELD_MISSING_DIAGNOSTIC: &str = "unrecognized subcommand 'held'";

/// The part of `engram work core held --json` TermAl reads: every item this
/// session holds a live claim on, newest claim first. Fields TermAl does not
/// read are ignored.
#[derive(Debug, Deserialize)]
struct EngramHeldClaims {
    items: Vec<EngramHeldClaim>,
    /// Held claims left out of `items` by Engram's row limit (the oldest).
    #[serde(default)]
    omitted: u64,
}

#[derive(Debug, Deserialize)]
struct EngramHeldClaim {
    #[serde(default)]
    work_id: String,
    #[serde(default)]
    focused: bool,
    /// The binding `session_bind` would accept for this claim, or null when
    /// bind would refuse it (a pending handoff offer, a run no longer claimed
    /// or active). A revision by the holder re-accepts the claim, so it shows
    /// a fresh binding at the new revision rather than null. Absent reads as
    /// null.
    #[serde(default)]
    control_binding: Option<EngramControlWorkBinding>,
}

/// What a work-binding read prefers and what it leaves out
/// (`select_engram_held_binding`).
#[derive(Clone, Copy, Debug, Default)]
struct EngramBindingPreference<'a> {
    /// The binding the session is bound with now, which the selection keeps
    /// while that claim is still bindable.
    current: Option<&'a EngramControlWorkBinding>,
    /// The bindings Engram refused as stale for the session, while each one's
    /// retry window lasts (`engram_refused_work_bindings_in_window`): the
    /// selection leaves them out, so a claim Engram refuses gives way to
    /// another the session holds rather than unbinding it.
    refused: &'a [EngramControlWorkBinding],
}

/// Reads the work binding a session should be bound with, under the agent's
/// own Engram session id. `engram work core held` lists the claims this
/// session holds, in one read-only snapshot that selects no focus, stages no
/// delivery and appends nothing. Unlike the focus-based read it replaced, it
/// finds a claim wherever the agent's focus is: any agent verb that names
/// another item moves focus, and a note on another item must not unbind the
/// session. `preference` names the binding the session is bound with now and
/// the one Engram refused (`select_engram_held_binding`).
fn read_engram_work_binding_from_cli(
    connection: &EngramConnectionConfig,
    preference: EngramBindingPreference<'_>,
    timeout: Duration,
    trace_boot_recovery: bool,
) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
    let started_at = std::time::Instant::now();
    let mut args = vec![
        "work",
        "--actor-id",
        connection.actor_id.as_str(),
        "--session-id",
        connection.session_id.as_str(),
    ];
    if let Some(actor_context) = connection.actor_context.as_deref() {
        args.extend(["--actor-context", actor_context]);
    }
    args.extend(["core", "held", "--json"]);
    let held = run_engram_json_command_with_lock_retry(
        connection,
        &args,
        timeout,
        ENGRAM_WORK_BINDING_READER_LABEL,
    );
    if trace_boot_recovery {
        log_engram_boot_recovery_phase(
            &connection.session_id,
            "work_core_held",
            1,
            started_at.elapsed(),
            &held,
        );
    }
    let held = held.map_err(|mut error| {
        // Every bind reads the held claims, so an Engram build that predates
        // the command fails every bind; say what would fix it.
        if error
            .message
            .contains(ENGRAM_WORK_CORE_HELD_MISSING_DIAGNOSTIC)
        {
            error.message.push_str(
                "; this Engram build has no `work core held`, which binding a session to \
                 claimed work needs: install a newer Engram",
            );
        }
        error
    })?;
    let held: EngramHeldClaims = serde_json::from_value(held).map_err(|error| {
        EngramTransportError::protocol(format!("invalid Engram work core held output: {error}"))
    })?;
    Ok(select_engram_held_binding(
        held.items,
        held.omitted,
        preference,
    ))
}

/// The binding to bind from the claims a session holds. Only a claim with a
/// binding is a candidate; bind would refuse the others, and the bindings
/// Engram refused as stale (`preference.refused`) are left out while their
/// retry windows last, since Engram validates more than the listing shows.
/// The focused claim comes first, as the item the agent declared it is
/// working on; then the claim the session is bound to now
/// (`preference.current`), with its current revision and fence, so focus
/// moving to an unclaimed item, or to one Engram refused, does not switch a
/// session that holds several claims; then the most recently claimed, which
/// Engram lists first. When Engram left claims out of the list (`omitted`),
/// a current claim not listed may be among them, so the current binding is
/// kept rather than switched, unless it is a refused one; if the claim is
/// in fact gone, Engram refuses the binding as stale and the refusal heal
/// reads again. No candidate binds no work. A control session carries one
/// binding, so a turn's evidence lands on that claim's run only.
fn select_engram_held_binding(
    held: Vec<EngramHeldClaim>,
    omitted: u64,
    preference: EngramBindingPreference<'_>,
) -> Option<EngramControlWorkBinding> {
    let current = preference.current;
    let current_listed =
        current.is_some_and(|current| held.iter().any(|claim| claim.work_id == current.work_id));
    let candidates = held
        .into_iter()
        .filter_map(|claim| {
            claim
                .control_binding
                .filter(|binding| !preference.refused.contains(binding))
                .map(|binding| (claim.focused, binding))
        })
        .collect::<Vec<_>>();
    if let Some((_, binding)) = candidates.iter().find(|(focused, _)| *focused) {
        return Some(binding.clone());
    }
    if let Some(current) = current {
        if let Some((_, binding)) = candidates
            .iter()
            .find(|(_, binding)| binding.work_id == current.work_id)
        {
            return Some(binding.clone());
        }
        if omitted > 0 && !current_listed && !preference.refused.contains(current) {
            return Some(current.clone());
        }
    }
    candidates.into_iter().next().map(|(_, binding)| binding)
}
