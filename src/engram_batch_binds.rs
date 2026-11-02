// Captured members of an ordinary carried launch. Historical owed-bind
// records remain raw session history; no retry or send owner is restored.

/// The members of a full gate launched in `root` by the session at `index`,
/// bound to its claim: the other items it named on the same worktree
/// (`engram_bind_targets_at_launch`), each with the fence its claim has in
/// `held`, the session's held claims read at the launch. With no read
/// (`None`), the members only, with their root entries' fences: a launch
/// whose read failed binds none of them. None when the session is unbound,
/// its claim names no root, or the gate ran outside that root.
fn engram_launch_bind_targets(
    inner: &StateInner,
    index: usize,
    session_id: &str,
    root: &FsPath,
    held: Option<&EngramHeldClaims>,
) -> Vec<EngramBindTarget> {
    let Some(store) = engram_project_for_session_locked(inner, session_id)
        .and_then(|project| project.engram.as_ref())
        .and_then(|settings| settings.authority_store_key.as_ref())
    else {
        return Vec::new();
    };
    let Some(binding) = inner.sessions[index].engram.work_binding.as_ref() else {
        return Vec::new();
    };
    let Some(launch) = engram_work_source_root_for_claim(
        &inner.engram_work_source_roots,
        store,
        &binding.work_id,
        &binding.claim_id,
    ) else {
        return Vec::new();
    };
    if engram_path_key(FsPath::new(&launch.root)) != engram_path_key(root) {
        return Vec::new();
    }
    let unread = EngramHeldClaims {
        items: Vec::new(),
        omitted: 0,
    };
    engram_bind_targets_at_launch(
        &inner.engram_work_source_roots,
        launch,
        session_id,
        held.unwrap_or(&unread),
    )
}

/// Whether the command whose check `check` ends with `exit` launches a gate
/// that is carried and has batch members, so the session's held claims are
/// read at the launch (`engram_launch_batch_members`).
fn engram_launch_needs_held_read(
    inner: &StateInner,
    index: usize,
    session_id: &str,
    check: &EngramTurnCheck,
    exit: Option<EngramCommandExit>,
) -> bool {
    engram_check_is_launcher_full(&check.command)
        && matches!(
            engram_launch_disposition(&check.command, exit.unwrap_or(EngramCommandExit::Unknown)),
            EngramLaunchDisposition::Carry
        )
        && !engram_launch_bind_targets(inner, index, session_id, &check.target.root, None)
            .is_empty()
}

/// The members a gate launched at `check` records, each with the fence its
/// claim has in `launch_held`, the session's held claims read at the launch,
/// and a line for each member whose fence at launch is not known: every
/// member when the read failed or was not made, and one missing from a list
/// Engram cut short (`omitted`), which may still be held. A member missing
/// from a complete list keeps its root entry's fence as captured provenance;
/// this snapshot creates no binding operation.
fn engram_launch_batch_members(
    inner: &StateInner,
    index: usize,
    session_id: &str,
    check: &EngramTurnCheck,
    launch_held: Option<&std::result::Result<EngramHeldClaims, EngramTransportError>>,
) -> (Vec<EngramBindTarget>, Vec<String>) {
    let members = engram_launch_bind_targets(inner, index, session_id, &check.target.root, None);
    let unknown = |target: &EngramBindTarget, why: &str| {
        engram_batch_not_recorded_line(
            &target.short_ref,
            &format!("its claim's fence at the batch gate's launch {why}"),
        )
    };
    let Some(Ok(held)) = launch_held else {
        let lines = members.iter().map(|target| unknown(target, "could not be read")).collect();
        return (Vec::new(), lines);
    };
    let mut targets = Vec::new();
    let mut lines = Vec::new();
    for mut target in members {
        match held
            .items
            .iter()
            .find(|claim| claim.work_id == target.work_id && claim.claim_id == target.claim_id)
        {
            Some(claim) => {
                target.claim_fence = claim
                    .control_binding
                    .as_ref()
                    .map_or(claim.claim_fence, |binding| binding.claim_fence);
                targets.push(target);
            }
            None if held.omitted > 0 => lines.push(unknown(
                &target,
                "is not known: Engram's list of held claims was cut short",
            )),
            None => targets.push(target),
        }
    }
    (targets, lines)
}

/// A carried launch's captured member lost its artifact observation across
/// restart. This diagnostic creates no bind or retry duty.
fn engram_batch_member_lost_line(short_ref: &str, when: &str) -> String {
    engram_batch_not_recorded_line(
        short_ref,
        &format!(
            "TermAl restarted {when}, so the batch gate's record could not be bound to it; run \
             this item's gate again"
        ),
    )
}
