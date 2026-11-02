// One-shot ordinary held-claim reads and selection of a current work binding.
// Naming, native verification and observations use the same claim identity;
// this read changes no focus and authorizes no model turn.

/// Names the caller in every failure message of a one-shot Engram CLI call.
const ENGRAM_WORK_BINDING_READER_LABEL: &str = "work-binding reader";

/// What an Engram build without `work core held` prints on stderr when asked
/// for it (its argument parser's refusal).
const ENGRAM_WORK_CORE_HELD_MISSING_DIAGNOSTIC: &str = "unrecognized subcommand 'held'";

/// The part of `engram work core held --json` TermAl reads: every item this
/// session holds a live claim on, newest claim first. Fields TermAl does not
/// read are ignored.
#[derive(Clone, Debug, Deserialize)]
struct EngramHeldClaims {
    items: Vec<EngramHeldClaim>,
    /// Held claims left out of `items` by Engram's row limit (the oldest).
    #[serde(default)]
    omitted: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct EngramHeldClaim {
    #[serde(default)]
    work_id: String,
    /// The item's short reference, which agents name it by.
    #[serde(default)]
    short_ref: String,
    /// The claim itself, read from the row: `control_binding` is null while a
    /// handoff offer is pending, yet the row is still the live claim. Empty
    /// from an Engram build that does not list it.
    #[serde(default)]
    claim_id: String,
    #[serde(default)]
    claim_fence: i64,
    #[serde(default)]
    focused: bool,
    /// The current canonical work association, or null for an unavailable
    /// association (a pending handoff offer or a run no longer claimed or
    /// active). A revision by the holder re-accepts the claim, so it shows
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
}

/// Reads the work binding a session should be bound with, under the agent's
/// own Engram session id. `engram work core held` lists the claims this
/// session holds, in one read-only snapshot that selects no focus, stages no
/// delivery and appends nothing. Unlike the focus-based read it replaced, it
/// finds a claim wherever the agent's focus is: any agent verb that names
/// another item moves focus, and a note on another item must not unbind the
/// session. `preference` names its current claim association
/// (`select_engram_held_binding`).
fn read_engram_work_binding_from_cli(
    connection: &EngramConnectionConfig,
    preference: EngramBindingPreference<'_>,
    timeout: Duration,
    host_workdir: &FsPath,
) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
    let held =
        read_engram_held_claims_from_cli(connection, timeout, host_workdir)?;
    Ok(select_engram_held_binding(
        held.items,
        held.omitted,
        preference,
    ))
}

/// Every claim the session holds, as `engram work core held --json` lists
/// them under the agent's own Engram session id: the rows and how many the
/// row limit left out. One read-only snapshot that selects no focus, stages
/// no delivery and appends nothing. The work-binding read selects one binding
/// from it; a work's source root (`engram_source_roots.rs`) reads the claims
/// themselves.
fn read_engram_held_claims_from_cli(
    connection: &EngramConnectionConfig,
    timeout: Duration,
    host_workdir: &FsPath,
) -> std::result::Result<EngramHeldClaims, EngramTransportError> {
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
        host_workdir,
    );
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
    engram_decode_value(held, "invalid Engram work core held output")
}

/// The binding to bind from the claims a session holds. Only a claim with a
/// binding is a candidate. A null canonical association never becomes a
/// binding merely because its claim row exists.
/// The focused claim comes first, as the item the agent declared it is
/// working on; then the claim the session is bound to now
/// (`preference.current`), with its current revision and fence, so focus
/// moving to an unclaimed item does not switch a
/// session that holds several claims; then the most recently claimed, which
/// Engram lists first. When Engram left claims out of the list (`omitted`),
/// a current claim not listed may be among them, so the current binding is
/// kept for the caller's exact canonical revalidation rather than switched.
/// Without a candidate there is no work association. Each captured report carries its own
/// original claim and run.
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
        if omitted > 0 && !current_listed {
            return Some(current.clone());
        }
    }
    candidates.into_iter().next().map(|(_, binding)| binding)
}
