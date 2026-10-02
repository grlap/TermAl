// Engram turn continuity: whether a workspace changed between two mediated
// turns, kept apart from what either turn reports about itself.
//
// Owns: the anchor, which is where the last checkpoint Engram acknowledged
// left the workspace (its turn's closing basis, with the work, run and claim
// it reported to), and comparing it with the next turn's measured begin
// basis. The result is kept on the session for that turn and logged as a host
// diagnostic when the workspace drifted: written by another session, by a
// turn Claude Code started by itself, by the naming turn, or by anything else
// no turn reported.
//
// Does not own: a turn's own `source_changed`, which still compares that
// turn's measured begin with its close (`engram_turn_observations.rs`). The
// begin basis is never replaced by the anchor, so a change between turns is
// never attributed to the next turn. It also does not own reporting the drift
// to Engram, which waits on the producer representation agreed with Engram.
//
// New file; nothing was split out of another.

/// Where the last acknowledged checkpoint left a workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramContinuityAnchor {
    /// The grant the acknowledged checkpoint closed.
    grant_id: String,
    workspace_id: String,
    source_revision: String,
    observed_at: Option<String>,
    work_id: String,
    run_id: String,
    claim_id: String,
}

/// Why a turn's begin was not compared with the anchor. Never read as
/// "unchanged".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramContinuityGap {
    /// No checkpoint this process saw acknowledged left a basis.
    NoPreviousCheckpoint,
    /// The previous checkpoint reported to another work, run or claim.
    BindingChanged,
    /// The previous checkpoint measured another workspace (a source root
    /// named, renamed or cleared in between).
    WorkspaceChanged,
    /// This turn's begin basis could not be taken.
    BeginUnmeasured,
}

/// How a turn's measured begin compares with the anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramTurnContinuity {
    /// The workspace is as the previous acknowledged checkpoint left it.
    Unchanged { previous_grant_id: String },
    /// The workspace changed between the two turns, by a writer no turn
    /// reported. The cause is unknown to the host.
    Drifted {
        previous_grant_id: String,
        workspace_id: String,
        previous_revision: String,
        previous_observed_at: Option<String>,
        begin_revision: String,
        begin_observed_at: String,
    },
    NotCompared(EngramContinuityGap),
}

/// Compares a turn's measured `begin` basis, under `binding`, with `anchor`.
fn engram_turn_continuity(
    anchor: Option<&EngramContinuityAnchor>,
    binding: &EngramControlWorkBinding,
    begin: Option<&EngramExecutionSourceBasis>,
    begin_observed_at: &str,
) -> EngramTurnContinuity {
    let Some(anchor) = anchor else {
        return EngramTurnContinuity::NotCompared(EngramContinuityGap::NoPreviousCheckpoint);
    };
    if anchor.work_id != binding.work_id
        || anchor.run_id != binding.run_id
        || anchor.claim_id != binding.claim_id
    {
        return EngramTurnContinuity::NotCompared(EngramContinuityGap::BindingChanged);
    }
    let Some(begin) = begin else {
        return EngramTurnContinuity::NotCompared(EngramContinuityGap::BeginUnmeasured);
    };
    if begin.workspace_id != anchor.workspace_id {
        return EngramTurnContinuity::NotCompared(EngramContinuityGap::WorkspaceChanged);
    }
    if begin.source_revision == anchor.source_revision {
        return EngramTurnContinuity::Unchanged {
            previous_grant_id: anchor.grant_id.clone(),
        };
    }
    EngramTurnContinuity::Drifted {
        previous_grant_id: anchor.grant_id.clone(),
        workspace_id: anchor.workspace_id.clone(),
        previous_revision: anchor.source_revision.clone(),
        previous_observed_at: anchor.observed_at.clone(),
        begin_revision: begin.source_revision.clone(),
        begin_observed_at: begin_observed_at.to_owned(),
    }
}

/// The anchor a checkpoint closing `grant_id` leaves once Engram acknowledges
/// it: the closing basis of the turn's own observation in `report`, under
/// `binding`. `None` when the report carried no closing basis, so the anchor
/// is cleared rather than kept at an older checkpoint.
fn engram_continuity_anchor_from_report(
    session_id: &str,
    grant_id: &str,
    report: &EngramTurnReport,
    binding: &EngramControlWorkBinding,
) -> Option<EngramContinuityAnchor> {
    let turn_observation_id = engram_turn_observation_id(session_id, grant_id);
    let observation = report
        .observations
        .iter()
        .find(|observation| observation.observation_id == turn_observation_id)?;
    let basis = observation.source_basis.as_ref()?;
    Some(EngramContinuityAnchor {
        grant_id: grant_id.to_owned(),
        workspace_id: basis.workspace_id.clone(),
        source_revision: basis.source_revision.clone(),
        observed_at: observation.observed_at.clone(),
        work_id: binding.work_id.clone(),
        run_id: binding.run_id.clone(),
        claim_id: binding.claim_id.clone(),
    })
}

/// Engram acknowledged the checkpoint closing `grant_id`: the anchor moves to
/// where that turn left the workspace. Called once per acknowledgement, under
/// the lock, before the grant is cleared.
fn advance_engram_continuity_anchor(record: &mut SessionRecord, session_id: &str, grant_id: &str) {
    let anchor = match (
        record.engram.active_turn_report.as_ref(),
        record.engram.work_binding.as_ref(),
    ) {
        (Some((report_grant_id, report)), Some(binding)) if report_grant_id == grant_id => {
            engram_continuity_anchor_from_report(session_id, grant_id, report, binding)
        }
        _ => None,
    };
    record.engram.continuity_anchor = anchor;
}

/// Records how the turn `grant_id` began compared with the anchor, and logs a
/// drift as a host diagnostic. Called under the lock once the turn's begin
/// basis is taken; the begin basis itself is left as measured.
fn record_engram_turn_continuity(record: &mut SessionRecord, session_id: &str, grant_id: &str) {
    let Some(binding) = record.engram.work_binding.as_ref() else {
        record.engram.active_turn_continuity = None;
        return;
    };
    let begin_observed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let continuity = engram_turn_continuity(
        record.engram.continuity_anchor.as_ref(),
        binding,
        record.engram.active_turn_start_basis.as_ref(),
        &begin_observed_at,
    );
    if let EngramTurnContinuity::Drifted {
        previous_grant_id,
        workspace_id,
        previous_revision,
        previous_observed_at,
        begin_revision,
        begin_observed_at,
    } = &continuity
    {
        eprintln!(
            "engram> session={session_id} grant={grant_id} workspace {workspace_id} changed \
             between turns: the checkpoint of grant {previous_grant_id} left {previous_revision} \
             (at {}), this turn began at {begin_revision} (at {begin_observed_at}); no turn \
             reported the change, and its cause is unknown",
            previous_observed_at.as_deref().unwrap_or("an unknown time"),
        );
    }
    record.engram.active_turn_continuity = Some((grant_id.to_owned(), continuity));
}
