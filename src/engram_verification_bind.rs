// Binding one natively credited full gate to the other items of a batch.
//
// Owns: which other held items share a gate's worktree when it is launched,
// whether each may be bound when the gate settles, the verification_bind
// request for one item with its stable idempotency key, how Engram's answer
// settles that item, and the line its holder is told.
//
// Does not own: the gate's native credit and every fence on it
// (`engram_carried_checks.rs`), sending the request, persisting what is owed
// or replaying it after a restart, or the acceptance evaluation that cites the
// bound record, which Engram serves from the item's own run.
//
// A new module, included from main.rs beside `engram_carried_checks.rs`.
//
// A batch is declared by holding it: the session that runs the combined gate
// holds each member's claim, and each member's named source root is the
// gate's worktree. Engram's `verification_bind` then writes, on each member's
// own run, a passed record bound from the gate's native one, after checking
// that member again itself (Engram docs/features/acceptance-evaluation.md and
// behavioral-control-plane.md, verification_bind). One member per call, so one
// refusal never blocks another.

/// What the holder is told to do when a member has no sighting at the gate's
/// revision: Engram binds only a member whose newest sighting is there, and
/// the host never makes one up. A focus switch alone records nothing; a
/// granted turn on the member's own claim reports its root's source basis.
const ENGRAM_BIND_SIGHTING_REMEDY: &str =
    "before the gate, give this item a granted turn on its own claim in the gate's worktree on the frozen tree, so Engram records its newest sighting at the gate's revision";

/// One other held item of a batch, as the gate's launch saw it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramBindTarget {
    work_id: String,
    short_ref: String,
    claim_id: String,
    claim_fence: i64,
    /// The generation its named root had at launch: a rename or clear after
    /// it moves the generation, and the item is no longer bound.
    root_generation: u64,
}

/// The other items a gate launched in `launch`'s root may be bound to: every
/// other root entry this session named for the same worktree (the same root
/// path and the same common Git directory) in the same store. A claim the
/// session names there later is not one of them. Each member's fence is the
/// one its claim has at launch, as `held` lists it: a root entry keeps the
/// fence it was named at through a recovery, so a member recovered before
/// the launch is judged by its fence then. A member not held at launch keeps
/// its entry's fence, and settlement tells its holder it is no longer held.
/// Every member is kept: each is bound by a call of its own, so Engram's
/// limit on one request's targets never drops one.
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

/// Whether a member is bound when its gate settles.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramBindDecision {
    Bind {
        binding: EngramControlWorkBinding,
        sighting: EngramBindSighting,
    },
    /// Not bound, and why, in words its holder is told.
    Skip(String),
    /// Not decidable now: its claim is missing from a list of held claims
    /// Engram cut short, so it may still be held. Read again later.
    Unknown(String),
}

/// Whether `target` may be bound to a gate settled at `gate`, from what the
/// host reads at settlement: the claims the session holds, its own root
/// entries, and the member's newest sighting (Engram's
/// `named_root_sighting_read` at the head of its run). Engram decides again
/// when it binds; this keeps a request it would refuse from being sent and
/// says why in the holder's terms. A sighting is only ever read, never made.
fn engram_bind_decision(
    target: &EngramBindTarget,
    held: &EngramHeldClaims,
    roots: &[EngramWorkSourceRoot],
    sighting: std::result::Result<&EngramNamedRootSightingRead, &str>,
    gate: &EngramExecutionSourceBasis,
) -> EngramBindDecision {
    let skip = |reason: String| EngramBindDecision::Skip(reason);
    let Some(claim) = held
        .items
        .iter()
        .find(|claim| claim.work_id == target.work_id && claim.claim_id == target.claim_id)
    else {
        // Engram lists only its newest held claims: one missing from a list
        // it cut short may still be held, so nothing final is said of it.
        if held.omitted > 0 {
            return EngramBindDecision::Unknown(
                "its claim is not among the held claims Engram listed (the list was cut short)"
                    .to_owned(),
            );
        }
        return skip("its claim is no longer held by this session".to_owned());
    };
    // A held row without a binding is a claim with a handoff offer pending.
    let Some(binding) = claim.control_binding.clone() else {
        return skip("its claim has a handoff offer pending".to_owned());
    };
    if binding.claim_fence != target.claim_fence {
        return skip(format!(
            "its claim moved since the gate's launch (fence {} then, {} now)",
            target.claim_fence, binding.claim_fence
        ));
    }
    let named_here = roots.iter().any(|entry| {
        entry.work_id == target.work_id
            && entry.claim_id == target.claim_id
            && entry.generation == target.root_generation
            && entry.root == gate.workspace_id
    });
    if !named_here {
        return skip(
            "its named source root was renamed or cleared, or names another worktree, since the gate's launch"
                .to_owned(),
        );
    }
    let read = match sighting {
        Ok(read) => read,
        Err(why) => return skip(format!("its newest sighting could not be read ({why})")),
    };
    if read.work_id != target.work_id || read.run_id != binding.run_id {
        return skip("the sighting read names another item or run".to_owned());
    }
    if read.binding_changed {
        return skip("Engram's binding of its root moved at the head of its run".to_owned());
    }
    let EngramSightingRoot::Bound {
        workspace_id,
        generation,
        sighting,
        ..
    } = &read.root
    else {
        return skip("Engram binds no named root for it".to_owned());
    };
    if workspace_id != &gate.workspace_id
        || u64::try_from(*generation).ok() != Some(target.root_generation)
    {
        return skip("Engram binds it to another root or generation than the gate's".to_owned());
    }
    match sighting {
        EngramRootSighting::Present { record, revision, .. }
            if revision == &gate.source_revision =>
        {
            EngramBindDecision::Bind {
                binding,
                sighting: EngramBindSighting {
                    observation: record.clone(),
                    source_revision: revision.clone(),
                },
            }
        }
        EngramRootSighting::Present { revision, .. } => skip(format!(
            "its newest sighting is at {revision}, not at the gate's revision {}: {ENGRAM_BIND_SIGHTING_REMEDY}",
            gate.source_revision
        )),
        EngramRootSighting::Absent => skip(format!(
            "it has no sighting of its root: {ENGRAM_BIND_SIGHTING_REMEDY}"
        )),
    }
}

/// The idempotency key of binding `original` to `work_id`: one per (gate,
/// member), and the same on every resend, so Engram replays a request it
/// already committed instead of binding twice.
fn engram_bind_idempotency_key(original: &str, work_id: &str) -> String {
    format!("termal-verification-bind:{original}:{work_id}")
}

/// The request binding `original` to one member. It is built once and kept:
/// a resend sends exactly it, `measured_at` included, and after Engram
/// refuses the connection's credentials it is sent again with only the
/// routing token refreshed, which is not part of the intent Engram compares.
fn engram_verification_bind_request(
    routing_token: &str,
    original: &str,
    measurement: EngramBindMeasurement,
    binding: EngramControlWorkBinding,
    sighting: EngramBindSighting,
) -> EngramControlRequest {
    EngramControlRequest::VerificationBind {
        routing_token: routing_token.to_owned(),
        idempotency_key: engram_bind_idempotency_key(original, &binding.work_id),
        original: original.to_owned(),
        measurement,
        targets: vec![EngramVerificationBindTarget {
            binding,
            sighting,
            criteria: Vec::new(),
        }],
    }
}

// Outside tests nothing reads a bound member's record id or replay flag.
#[cfg_attr(not(test), allow(dead_code))]
/// How one `verification_bind` call settled its member.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramBindOutcome {
    /// Engram wrote the bound record (or replays the one it wrote).
    Bound { verification: String, replayed: bool },
    /// The member is bound from this gate already: Engram refused a second
    /// binding of it.
    AlreadyBound,
    /// Engram refused to bind the member, with a typed refusal that wrote
    /// nothing and that the same request meets again: it stays unbound.
    Final(String),
    /// Engram rejected this bind as an identity collision: an earlier,
    /// different operation already holds its key
    /// (`control_operation_idempotency_conflict`). Engram compares the stored
    /// intent before any bind write, so this request was rejected unapplied,
    /// but what the earlier operation is, or did, is not known. Terminal:
    /// never bound, never sent again under this key or a new one; its line
    /// names the key and the rejected bind's provenance.
    Collision(String),
    /// Nothing is known to be written: send the identical request again.
    Resend(String),
    /// Engram refused this connection's credentials. That says nothing of an
    /// earlier attempt, which may have committed: refresh the session's
    /// routing token and send the same request again, which Engram replays
    /// if it committed.
    Refresh(String),
}

/// Engram's error codes that a resend of the same request can settle (Engram
/// src/host.rs `store_error_code`): an expiry Engram certifies wrote nothing,
/// a busy store, and the generic storage error, which covers a failed commit.
const ENGRAM_BIND_RESEND_CODES: &[&str] = &[
    "control_request_expired",
    "control_store_busy",
    "storage_error",
];

/// Engram's refusals of a bind that provably wrote nothing (Engram
/// docs/features/acceptance-evaluation.md, `verification_bind`: "Any failure
/// refuses the whole request with `verification_bind_refused`, and nothing
/// is written for any target"; and a request it cannot decode at all). Any
/// other code is no proof that nothing was written: the identical request is
/// sent again, bounded, and an unconfirmed bind is disclosed as such.
const ENGRAM_BIND_PRE_WRITE_REFUSALS: &[&str] = &["verification_bind_refused", "invalid_request"];

/// The words every identity-collision line carries
/// (`EngramBindOutcome::Collision`). A settled entry whose line has them is
/// also written to the session's transcript, durably, in the same commit
/// that retires it (`apply_engram_owed_binds`).
const ENGRAM_BIND_COLLISION_PHRASE: &str = "Engram rejected this bind as an identity collision";

/// How `result`, the answer to binding one member `short_ref`, settles it.
/// A refusal of the connection is resent under refreshed credentials. Only
/// Engram's typed refusals that wrote nothing (`ENGRAM_BIND_PRE_WRITE_REFUSALS`,
/// such as an Engram without `verification_bind` answering
/// `invalid_request`) are final. An idempotency conflict is a terminal
/// identity collision: this request was rejected unapplied, and an earlier,
/// different operation holds its key. Every other
/// answer, an expiry, a busy store, a storage error, a code this build does
/// not know, or one that did not reach Engram or came back unreadable, is
/// resent as it is, since Engram replays a committed request by its key and
/// never binds twice; the resend is bounded, and the holder is told when no
/// certain answer came.
fn engram_bind_outcome(
    result: std::result::Result<Value, EngramTransportError>,
    original: &str,
    short_ref: &str,
    work_id: &str,
) -> EngramBindOutcome {
    let error = match result {
        Ok(receipt) => {
            // Bound only by a receipt for this very request: the gate's own
            // record as original, and a record id Engram minted for the
            // member. Any other receipt says nothing certain.
            let for_this_gate =
                receipt.get("original").and_then(Value::as_str) == Some(original);
            let bound = receipt
                .get("bound")
                .and_then(Value::as_array)
                .and_then(|bound| {
                    bound.iter().find(|entry| {
                        entry.get("work_id").and_then(Value::as_str) == Some(work_id)
                    })
                })
                .and_then(|entry| entry.get("verification").and_then(Value::as_str))
                .filter(|verification| for_this_gate && is_engram_record_id(verification));
            return match bound {
                Some(verification) => EngramBindOutcome::Bound {
                    verification: verification.to_owned(),
                    replayed: receipt.get("replayed").and_then(Value::as_bool) == Some(true),
                },
                // A receipt that does not name the member does not say it
                // was not bound: sent again, Engram's replay of the key
                // settles it, or the bind gives up as unconfirmed.
                None => EngramBindOutcome::Resend(
                    "Engram's receipt does not name this item for this gate".to_owned(),
                ),
            };
        }
        Err(error) => error,
    };
    let code = error.code.as_deref().unwrap_or_default();
    let detail = if code.is_empty() {
        error.message.clone()
    } else {
        format!("{code}: {}", error.message)
    };
    if error.kind != EngramTransportErrorKind::Remote {
        return EngramBindOutcome::Resend(detail);
    }
    match code {
        "verification_bind_refused"
            if error
                .message
                .contains(&format!("target {short_ref} already_bound")) =>
        {
            EngramBindOutcome::AlreadyBound
        }
        // Named with its key and original, so the disclosure keeps the
        // rejected bind's identity; the owed entry adds its target provenance.
        "control_operation_idempotency_conflict" => EngramBindOutcome::Collision(format!(
            "key {}; original {original}; work {work_id}; {detail}",
            engram_bind_idempotency_key(original, work_id)
        )),
        // The connection is refused, not the bind: final for that routing
        // token only (`engram_code_invalidates_routing_token`).
        code if engram_code_invalidates_routing_token(code) => EngramBindOutcome::Refresh(detail),
        code if ENGRAM_BIND_RESEND_CODES.contains(&code) => EngramBindOutcome::Resend(detail),
        code if ENGRAM_BIND_PRE_WRITE_REFUSALS.contains(&code) => {
            EngramBindOutcome::Final(format!("Engram refused it ({detail})"))
        }
        // A code that proves nothing about what was written: the identical
        // request again, never a new identity.
        _ => EngramBindOutcome::Resend(detail),
    }
}

/// The line the gate's holder is told about one member, or `None` while a
/// resend, with or without refreshed credentials, is still owed.
fn engram_bind_notice_line(short_ref: &str, outcome: &EngramBindOutcome) -> Option<String> {
    match outcome {
        EngramBindOutcome::Bound { .. } | EngramBindOutcome::AlreadyBound => Some(format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} the full gate is also recorded for {short_ref} \
             (bound), on its own run."
        )),
        EngramBindOutcome::Final(reason) => Some(engram_batch_not_recorded_line(short_ref, reason)),
        EngramBindOutcome::Collision(detail) => Some(engram_batch_not_recorded_line(
            short_ref,
            &format!(
                "{ENGRAM_BIND_COLLISION_PHRASE}: an earlier, different operation already holds \
                 its key ({detail}); the bind is not sent again, and the earlier operation is \
                 not known"
            ),
        )),
        EngramBindOutcome::Resend(_) | EngramBindOutcome::Refresh(_) => None,
    }
}

/// The line telling a batch gate's member `short_ref`'s holder that the
/// gate's record is not bound to it, and `why`. Every member line is a line
/// about checks' credit (`ENGRAM_CHECK_CREDIT_LINE_PREFIX`), so a batch's
/// lines merge into one and never push a bind or name line out.
fn engram_batch_not_recorded_line(short_ref: &str, why: &str) -> String {
    format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} the full gate is not recorded for {short_ref}: {why}.")
}
