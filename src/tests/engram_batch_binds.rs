// Tests for what a passed batch full gate owes its other members
// (`engram_batch_binds.rs`).
// Owns: the pending batch a credited gate leaves, the owed binds its
// checkpoint's answer turns it into, the launch fences the launching
// checkpoint confirms, and each owed bind's step, backoff, cap and request.
// Does not own: whether a member may be bound or how Engram's answer
// classifies (engram_verification_bind.rs tests), or the turn and
// checkpoint flow around them.
// A new module, declared from tests/mod.rs.
use super::*;

const NOW: &str = "2026-10-18T23:30:00.000Z";

fn at(seconds: i64) -> chrono::DateTime<chrono::Utc> {
    engram_parse_time(NOW).unwrap() + chrono::Duration::seconds(seconds)
}

fn target(work: &str, fence: i64) -> EngramBindTarget {
    EngramBindTarget {
        work_id: format!("work-{work}"),
        short_ref: format!("w-{work}"),
        claim_id: format!("claim-{work}"),
        claim_fence: fence,
        root_generation: 7,
    }
}

fn measurement() -> EngramBindMeasurement {
    EngramBindMeasurement {
        workspace_id: r"\\?\C:\repo\.tmp\wt-batch".to_owned(),
        source_revision: "content-v1:batch".to_owned(),
        measured_at: NOW.to_owned(),
    }
}

fn pending(grant: &str, producer: &str, members: &[&str]) -> EngramPendingBatch {
    EngramPendingBatch {
        checkpoint_grant_id: grant.to_owned(),
        producer_observation_id: producer.to_owned(),
        measurement: measurement(),
        targets: members.iter().map(|work| target(work, 3)).collect(),
        pended_at: NOW.to_owned(),
    }
}

/// A report whose inputs are, in order, `inputs`: (producer id, kind,
/// producer outcome).
fn report(inputs: &[(&str, &str, &str)]) -> EngramTurnReport {
    serde_json::from_value(json!({
        "observations": inputs.iter().map(|(id, _, outcome)| json!({
            "observation_id": id,
            "action_fingerprint": "a".repeat(64),
            "effect": "observe",
            "outcome": outcome,
            "source_changed": false,
        })).collect::<Vec<_>>(),
        "verification_evidence": inputs.iter().map(|(id, kind, _)| json!({
            "producer_observation": { "kind": "observation_id", "observation_id": id },
            "check_kind": kind,
        })).collect::<Vec<_>>(),
    }))
    .unwrap()
}

fn receipt(grant: &str, ids: &[String]) -> EngramCheckpointReceipt {
    serde_json::from_value(json!({
        "grant_id": grant, "checkpoint": "d".repeat(64),
        "cursor": 3, "confirmed_cursor": 3, "verification_evidence": ids,
    }))
    .unwrap()
}

fn owed(work: &str) -> EngramOwedBind {
    EngramOwedBind {
        original: "1".repeat(64),
        measurement: measurement(),
        target: target(work, 3),
        decided: None,
        decision_unconfirmed: false,
        attempts: 0,
        first_owed_at: NOW.to_owned(),
        last_detail: None,
        next_attempt_at: None,
    }
}

#[test]
fn the_checkpoint_answer_owes_each_member_a_bind_from_the_gates_own_record() {
    let gate = engram_batch_producer_observation_id("session-a", "grant-launch", 0, "gate");
    // The producer id is the digest the turn report gives the check.
    assert_eq!(
        gate,
        sha256_hex("termal-turn-check:session-a:grant-launch:0:gate".as_bytes())
    );
    let mut pending_batches = vec![
        pending("grant-close", &gate, &["member-a", "member-b"]),
        pending("grant-later", &"9".repeat(64), &["member-c"]),
    ];
    // A Build input first: the gate's own record is the id at ITS index.
    let report = report(&[
        (&"8".repeat(64), "build", "succeeded"),
        (&gate, "test", "succeeded"),
    ]);
    let settlement = engram_owe_batch_binds(
        &mut pending_batches,
        &report,
        // Engram mints record ids as 32 hex digits.
        &receipt("grant-close", &["1".repeat(32), "2".repeat(32)]),
        NOW,
    );
    assert!(settlement.lines.is_empty(), "{:?}", settlement.lines);
    let own = "2".repeat(32);
    assert_eq!(
        settlement
            .owed
            .iter()
            .map(|owed| (owed.original.as_str(), owed.target.short_ref.as_str()))
            .collect::<Vec<_>>(),
        vec![(own.as_str(), "w-member-a"), (own.as_str(), "w-member-b")]
    );
    assert!(settlement.owed.iter().all(|owed| owed.attempts == 0
        && owed.decided.is_none()
        && owed.first_owed_at == NOW
        && owed.measurement == measurement()));
    // Another checkpoint's batch stays pending for its own answer.
    assert_eq!(pending_batches, vec![pending("grant-later", &"9".repeat(64), &["member-c"])]);
}

#[test]
fn an_answer_that_does_not_name_the_gates_passed_record_owes_nothing_and_says_so() {
    let gate = engram_batch_producer_observation_id("session-a", "grant-launch", 0, "gate");
    let lines = |report: &EngramTurnReport, ids: &[String]| {
        let mut pending_batches = vec![pending("grant-close", &gate, &["member-a"])];
        let settlement =
            engram_owe_batch_binds(&mut pending_batches, report, &receipt("grant-close", ids), NOW);
        assert!(settlement.owed.is_empty());
        assert!(pending_batches.is_empty(), "the answered batch is settled either way");
        settlement.lines
    };
    let passed = report(&[(&gate, "test", "succeeded")]);
    for ids in [
        Vec::new(),
        vec!["1".repeat(64), "2".repeat(64)],
        vec!["not-an-object-id".to_owned()],
        vec!["1".repeat(31)],
    ] {
        assert_eq!(
            lines(&passed, &ids),
            vec![
                "[TermAl] Check credit: the full gate is not recorded for w-member-a: Engram's \
                 checkpoint answer does not name the gate's own passed record."
                    .to_owned()
            ],
            "{ids:?}"
        );
    }
    // The gate is not in the report as a passed Test.
    assert_eq!(lines(&report(&[(&gate, "test", "failed")]), &["1".repeat(64)]).len(), 1);
    assert_eq!(lines(&report(&[(&gate, "build", "succeeded")]), &["1".repeat(64)]).len(), 1);
    assert_eq!(
        lines(&report(&[(&"7".repeat(64), "test", "succeeded")]), &["1".repeat(64)]).len(),
        1
    );
}

#[test]
fn an_owed_bind_loaded_after_a_restart_is_sent_again_however_long_the_host_was_down() {
    let mut stale = owed("member-a");
    stale.first_owed_at = "2026-10-17T00:00:00.000Z".to_owned();
    stale.attempts = 9;
    stale.next_attempt_at = Some("2026-10-17T00:10:00.000Z".to_owned());
    // Unadjusted, it would give up without one more send.
    assert!(matches!(engram_owed_bind_step(&stale, at(0)), EngramOwedBindStep::GiveUp(_)));
    let loaded = engram_owed_bind_after_load(stale.clone(), NOW);
    assert_eq!(loaded.first_owed_at, NOW);
    assert_eq!(loaded.next_attempt_at, None);
    assert_eq!(loaded.attempts, ENGRAM_OWED_BIND_MAX_ATTEMPTS - 1);
    assert_eq!((loaded.original, loaded.target), (stale.original, stale.target));
    assert_eq!(engram_owed_bind_step(&engram_owed_bind_after_load(owed("member-a"), NOW), at(0)),
        EngramOwedBindStep::Decide);
}

#[test]
fn an_owed_bind_is_decided_once_then_sent_with_backoff_until_its_cap() {
    let mut entry = owed("member-a");
    assert_eq!(engram_owed_bind_step(&entry, at(0)), EngramOwedBindStep::Decide);
    entry.decided = Some(EngramOwedBindDecided {
        binding: EngramControlWorkBinding {
            root_execution_id: "root-member-a".to_owned(),
            work_id: "work-member-a".to_owned(),
            run_id: "run-member-a".to_owned(),
            work_revision: 2,
            claim_id: "claim-member-a".to_owned(),
            claim_fence: 3,
        },
        sighting: EngramBindSighting {
            observation: "a".repeat(32),
            source_revision: "content-v1:batch".to_owned(),
        },
    });
    assert_eq!(engram_owed_bind_step(&entry, at(0)), EngramOwedBindStep::Send);
    // An uncertain answer counts an attempt and waits by the schedule.
    assert_eq!(
        engram_owed_bind_answered(&mut entry, EngramBindOutcome::Resend("busy".to_owned()), at(0)),
        None
    );
    assert_eq!((entry.attempts, entry.last_detail.as_deref()), (1, Some("busy")));
    assert_eq!(engram_owed_bind_step(&entry, at(4)), EngramOwedBindStep::Wait);
    assert_eq!(engram_owed_bind_step(&entry, at(5)), EngramOwedBindStep::Send);
    engram_owed_bind_answered(&mut entry, EngramBindOutcome::Refresh("superseded".to_owned()), at(5));
    assert_eq!(engram_owed_bind_step(&entry, at(19)), EngramOwedBindStep::Wait);
    assert_eq!(engram_owed_bind_step(&entry, at(20)), EngramOwedBindStep::Send);
    // A settling answer gives the holder its line.
    let mut bound = entry.clone();
    assert_eq!(
        engram_owed_bind_answered(&mut bound, EngramBindOutcome::AlreadyBound, at(20)).as_deref(),
        Some(
            "[TermAl] Check credit: the full gate is also recorded for w-member-a (bound), on \
             its own run."
        )
    );
    // Past eight attempts, or thirty minutes, it gives up with its last word:
    // unconfirmed once decided, since a decided bind may have been sent and
    // bound even if a crash took the record of the send...
    let mut tired = entry.clone();
    tired.attempts = 8;
    tired.next_attempt_at = None;
    assert_eq!(
        engram_owed_bind_step(&tired, at(60)),
        EngramOwedBindStep::GiveUp(
            "[TermAl] Check credit: the full gate is unconfirmed for w-member-a: no certain \
             answer after 8 attempts (superseded); it may or may not be bound."
                .to_owned()
        )
    );
    // ...and certainly not recorded when it was never decided, so never sent.
    let mut old = owed("member-a");
    old.attempts = 1;
    old.last_detail = Some("its claim could not be read".to_owned());
    assert_eq!(
        engram_owed_bind_step(&old, at(30 * 60 + 1)),
        EngramOwedBindStep::GiveUp(engram_batch_not_recorded_line(
            "w-member-a",
            "its bind was never sent (its claim could not be read); run this item's gate again"
        ))
    );
    assert_eq!(engram_owed_bind_step(&old, at(30 * 60)), EngramOwedBindStep::Decide);
}

#[test]
fn a_decision_is_never_sent_before_it_is_confirmed_stored() {
    let mut entry = owed("member-a");
    entry.decided = Some(EngramOwedBindDecided {
        binding: EngramControlWorkBinding {
            root_execution_id: "root-member-a".to_owned(),
            work_id: "work-member-a".to_owned(),
            run_id: "run-member-a".to_owned(),
            work_revision: 2,
            claim_id: "claim-member-a".to_owned(),
            claim_fence: 3,
        },
        sighting: EngramBindSighting {
            observation: "a".repeat(32),
            source_revision: "content-v1:batch".to_owned(),
        },
    });
    entry.decision_unconfirmed = true;
    // Decided in this process and not read back yet: confirmed, never sent,
    // so certainly not bound.
    assert_eq!(engram_owed_bind_step(&entry, at(0)), EngramOwedBindStep::Confirm);
    assert_eq!(engram_owed_bind_step(&entry, at(600)), EngramOwedBindStep::Confirm);
    assert!(!engram_owed_bind_may_be_bound(&entry));
    // Read back from the store, it is stored: sent as decided.
    let stored: EngramOwedBind =
        serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
    assert!(!stored.decision_unconfirmed);
    assert!(engram_owed_bind_may_be_bound(&stored), "a stored decision may have been sent");
    assert_eq!(stored.decided, entry.decided);
    assert_eq!(engram_owed_bind_step(&stored, at(0)), EngramOwedBindStep::Send);
}

#[test]
fn a_batchs_member_lines_merge_and_never_push_a_bind_or_name_line_out() {
    let mut engram = EngramSessionState::default();
    let named = "[TermAl] The source root for w-launch is .tmp/wt-batch.".to_owned();
    engram.set_pending_source_root_line(named.clone());
    for member in ["a", "b", "c", "d", "e", "f"] {
        engram.set_pending_source_root_line(engram_batch_not_recorded_line(
            &format!("w-{member}"),
            "its claim moved",
        ));
    }
    engram.set_pending_source_root_line(engram_batch_member_lost_line("w-g", "before it settled"));
    let pending = engram.pending_source_root_line.clone().unwrap_or_default();
    assert!(pending.contains(&named), "{pending}");
    assert!(pending.contains("w-g"), "{pending}");
    assert_eq!(pending.lines().count(), 2, "{pending}");
}

#[test]
fn a_refused_or_unconfirmed_checkpoint_drains_its_batches_with_a_line_per_member() {
    let mut pending_batches = vec![
        pending("grant-refused", &"9".repeat(64), &["member-a", "member-b"]),
        pending("grant-open", &"8".repeat(64), &["member-c"]),
    ];
    let lines = engram_drain_pending_batches(
        &mut pending_batches,
        |batch| batch.checkpoint_grant_id == "grant-refused",
        "Engram refused the checkpoint that carried the batch gate (stale_fence)",
    );
    assert_eq!(
        lines,
        vec![
            engram_batch_not_recorded_line(
                "w-member-a",
                "Engram refused the checkpoint that carried the batch gate (stale_fence)"
            ),
            engram_batch_not_recorded_line(
                "w-member-b",
                "Engram refused the checkpoint that carried the batch gate (stale_fence)"
            ),
        ]
    );
    assert_eq!(pending_batches, vec![pending("grant-open", &"8".repeat(64), &["member-c"])]);
    // Past thirty minutes without an answer, or with no time, it expires.
    assert!(!engram_pending_batch_expired(&pending_batches[0], at(30 * 60)));
    assert!(engram_pending_batch_expired(&pending_batches[0], at(30 * 60 + 1)));
    let mut timeless = pending_batches[0].clone();
    timeless.pended_at = String::new();
    assert!(engram_pending_batch_expired(&timeless, at(0)));
}

#[test]
fn every_send_of_an_owed_bind_is_the_same_request_with_the_current_routing_token() {
    let mut entry = owed("member-a");
    assert!(engram_owed_bind_request(&entry, "routing-1").is_none(), "not decided yet");
    entry.decided = Some(EngramOwedBindDecided {
        binding: EngramControlWorkBinding {
            root_execution_id: "root-member-a".to_owned(),
            work_id: "work-member-a".to_owned(),
            run_id: "run-member-a".to_owned(),
            work_revision: 2,
            claim_id: "claim-member-a".to_owned(),
            claim_fence: 3,
        },
        sighting: EngramBindSighting {
            observation: "a".repeat(32),
            source_revision: "content-v1:batch".to_owned(),
        },
    });
    let wire = |token: &str| {
        let mut value = serde_json::to_value(engram_owed_bind_request(&entry, token).unwrap()).unwrap();
        let token = value["routing_token"].take();
        (value, token)
    };
    let (first, first_token) = wire("routing-1");
    let (refreshed, refreshed_token) = wire("routing-2");
    assert_eq!(first, refreshed, "the intent never changes");
    assert_eq!((first_token, refreshed_token), (json!("routing-1"), json!("routing-2")));
    assert_eq!(first["original"], "1".repeat(64));
    assert_eq!(
        first["idempotency_key"],
        format!("termal-verification-bind:{}:work-member-a", "1".repeat(64))
    );
    // It survives a restart as it was: persisted and read back unchanged.
    let persisted = serde_json::to_string(&entry).unwrap();
    assert_eq!(serde_json::from_str::<EngramOwedBind>(&persisted).unwrap(), entry);
}
