// Launch-member provenance and retained verification-bind protocol shapes.
//
// The live helper captures the other named items sharing a carried gate's
// worktree at launch (`engram_batch_binds.rs`). That snapshot is history:
// it creates no binding, send, settlement or retry duty.
//
// Retained request and receipt data shapes do not grant credit; there is no
// verification_bind decision, sender or retry owner. New registered
// committed-input runs use the independent verification_record sender
// (`engram_verification_record.rs`); acceptance
// evidence still reads bound records Engram already holds
// (`acceptance_evidence_selection.rs`).
//
// Included from main.rs beside the ordinary carried-check owner.

/// One other held item of a batch, as the gate's launch saw it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramBindTarget {
    work_id: String,
    short_ref: String,
    claim_id: String,
    claim_fence: i64,
    /// The named-root generation captured at launch, retained as provenance.
    root_generation: u64,
}

/// The other named items captured for a gate launched in `launch`'s root: every
/// other root entry this session named for the same worktree (the same root
/// path and the same common Git directory) in the same store. A claim the
/// session names there later is not one of them. Each member's fence is the
/// one its claim has at launch, as `held` lists it: a root entry keeps the
/// fence it was named at through a recovery, so a member recovered before
/// the launch is judged by its fence then. A member not held at launch keeps
/// its entry's fence as captured provenance. Every matching member is kept;
/// this helper neither binds a member nor sends a request.
fn engram_bind_targets_at_launch(
    roots: &[EngramWorkSourceRoot],
    launch: &EngramWorkSourceRoot,
    session_id: &str,
    held: &EngramHeldClaims,
) -> Vec<EngramBindTarget> {
    let mut targets: Vec<EngramBindTarget> = Vec::new();
    for entry in roots {
        let member = entry.store == launch.store
            && entry.root == launch.root
            && entry.common_dir_key == launch.common_dir_key
            && entry.named_by_session == session_id
            && entry.work_id != launch.work_id;
        if member && !targets.iter().any(|target| target.work_id == entry.work_id) {
            let claim_fence = held
                .items
                .iter()
                .find(|claim| claim.work_id == entry.work_id && claim.claim_id == entry.claim_id)
                .map(|claim| {
                    claim
                        .control_binding
                        .as_ref()
                        .map_or(claim.claim_fence, |binding| binding.claim_fence)
                })
                .unwrap_or(entry.claim_fence);
            targets.push(EngramBindTarget {
                work_id: entry.work_id.clone(),
                short_ref: entry.short_ref.clone(),
                claim_id: entry.claim_id.clone(),
                claim_fence,
                root_generation: entry.generation,
            });
        }
    }
    targets
}

/// A member's newest sighting, as `verification_bind` names it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramBindSighting {
    observation: String,
    source_revision: String,
}

/// The host's measurement of the gate's root, as `verification_bind` takes
/// it: the gate's settled source basis and when it was taken.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramBindMeasurement {
    workspace_id: String,
    source_revision: String,
    measured_at: String,
}

/// One member of a `verification_bind` request.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramVerificationBindTarget {
    binding: EngramControlWorkBinding,
    sighting: EngramBindSighting,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    criteria: Vec<u32>,
}

/// The line telling a batch gate's member `short_ref`'s holder that the
/// gate's record is not bound to it, and `why`. Every member line is a line
/// about checks' credit (`ENGRAM_CHECK_CREDIT_LINE_PREFIX`), so a batch's
/// lines merge into one and never push a bind or name line out.
fn engram_batch_not_recorded_line(short_ref: &str, why: &str) -> String {
    format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} the full gate is not recorded for {short_ref}: {why}."
    )
}
