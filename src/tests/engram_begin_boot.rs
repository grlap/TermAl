//! Restart witnesses for the exact begin replay: a begin-unknown head saved
//! by this host (new format, with its prepared begin) or by a host before it
//! (old format: a retained evaluate and an uncertain grant only) is resolved
//! after a reload without Resume, and its retained message is delivered once.
//! Does not own the running host's replay (engram_begin_replay.rs, whose
//! scripted producer and helpers this module uses as a child of it).

use super::*;

/// The restart: the saved record replaces the live one, as a reload does,
/// after `edit` shapes what the earlier host saved, then boot preparation.
fn restart(state: &AppState, session: &str, edit: impl FnOnce(&mut PersistedSessionRecord)) {
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        let mut saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        edit(&mut saved);
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
    }
    state.prepare_engram_sessions_for_boot_recovery().unwrap();
}

/// What a host before the prepared begin saved for a begin-unknown head: its
/// retained evaluate and uncertain grant, no prepared begin, and no retry.
fn pre_fix(saved: &mut PersistedSessionRecord) {
    saved.queued_prompts[0]
        .engram_evaluate
        .as_mut()
        .expect("the retained evaluate")
        .prepared_begin = None;
    saved.engram_admission_retry = None;
}

fn restart_checkpoints(transport: &ScriptedReplay) -> usize {
    transport
        .producer
        .requests()
        .iter()
        .filter(|recorded| {
            recorded.request["operation"] == "turn_checkpoint"
                && recorded.request["idempotency_key"]
                    .as_str()
                    .is_some_and(|key| key.starts_with("termal-restart-checkpoint:"))
        })
        .count()
}

/// How many requests reached the producer so far, read before a restart.
fn sent(transport: &ScriptedReplay) -> usize {
    transport.producer.requests().len()
}

/// After the restart, the exact recovery (`first`: the replayed begin, or the
/// replayed retained evaluate) reaches the producer before any status read,
/// restart checkpoint or rebind could settle the head another way.
fn assert_recovery_comes_first(transport: &ScriptedReplay, sent_before: usize, first: &str) {
    let operations: Vec<String> = transport
        .producer
        .requests()
        .iter()
        .skip(sent_before)
        .map(|recorded| recorded.request["operation"].as_str().unwrap_or_default().to_owned())
        .collect();
    let position = operations
        .iter()
        .position(|operation| operation == first)
        .unwrap_or_else(|| panic!("no {first} after the restart: {operations:?}"));
    assert!(
        operations[..position].iter().all(|operation| !matches!(
            operation.as_str(),
            "session_status" | "turn_checkpoint" | "session_bind"
        )),
        "nothing competes before the {first}: {operations:?}"
    );
}

fn evaluate_keys(transport: &ScriptedReplay) -> Vec<Value> {
    evaluates(transport)
        .iter()
        .map(|evaluate| evaluate["idempotency_key"].clone())
        .collect()
}

/// Acknowledges the scheduled replay record before it is due, as the tick
/// does on the running host, so the saved record is the acknowledged one.
fn acknowledge_before_due(state: &AppState) {
    state.engram_abort_retry_tick(chrono::Utc::now());
}

fn settle_after_restart(state: &AppState, session: &str) {
    for _ in 0..4 {
        tick_past_due(state, session);
    }
}

/// New format, applied begin. The begin was applied and its reply lost; the
/// host restarts with the replay scheduled. Producer status now reports the
/// grant begun, which the cold recovery used to close with a restart
/// checkpoint and an interrupted hold. Instead the rebuilt replay keeps its
/// cadence, the exact begin is replayed, and the head is delivered once.
#[test]
fn begin_boot_new_format_applied_begin_is_delivered_once_after_restart() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.status_open_grant_state.lock().unwrap() = Some("begun".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    acknowledge_before_due(&state);
    let before = with_record(&state, &session, |record| record.engram.admission_retry.clone())
        .expect("the replay is scheduled");
    assert!(before.acknowledged);

    let sent_before = sent(&transport);
    restart(&state, &session, |_| {});
    let rebuilt = with_record(&state, &session, |record| record.engram.admission_retry.clone());
    assert_eq!(rebuilt.as_ref(), Some(&before), "cadence and owner kept");
    settle_after_restart(&state, &session);

    assert_recovery_comes_first(&transport, sent_before, "turn_begin");
    assert_eq!(restart_checkpoints(&transport), 0, "no restart checkpoint closes it");
    assert_every_begin_is(&transport, &first);
    assert_eq!(begins(&transport).len(), 2, "one exact begin replay");
    assert_eq!(evaluates(&transport).len(), 1, "no evaluate after the restart");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
    assert_eq!(retained(&state, &session), 0);
}

/// New format, expired unbegun grant. The first begin never reached Engram;
/// after the restart the replay is refused grant_expired, so the head proceeds
/// through a fresh evaluate under a new key and is delivered once.
#[test]
fn begin_boot_new_format_expired_unbegun_grant_is_delivered_once_after_restart() {
    let script = ReplayScript::default();
    script.unavailable.store(1, Ordering::SeqCst);
    *script.status_open_grant_state.lock().unwrap() = Some("issued".to_owned());
    let producer = StatefulEngramControlTransport::with_first_begin_refusal("grant_expired");
    let (state, session, receiver, transport) = replay_fixture_with(producer, script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    acknowledge_before_due(&state);

    let sent_before = sent(&transport);
    restart(&state, &session, |_| {});
    settle_after_restart(&state, &session);

    assert_recovery_comes_first(&transport, sent_before, "turn_begin");
    assert_eq!(restart_checkpoints(&transport), 0);
    let keys = evaluate_keys(&transport);
    assert_eq!(keys.len(), 2, "one fresh evaluate: {keys:?}");
    assert_ne!(keys[0], keys[1]);
    let last = begins(&transport).last().cloned().unwrap();
    assert_ne!(last["grant_id"], first["grant_id"], "never resurrected");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
    assert_eq!(retained(&state, &session), 0);
}

/// Old format, applied begin. A host before the prepared begin saved only the
/// retained evaluate and the uncertain grant. After the restart the exact
/// retained evaluate is replayed; Engram returns its stored grant, which is the
/// uncertain one, so the begin is rebuilt from that producer answer (its key
/// derived from the evaluate's), replayed, settled by its stored receipt, and
/// the head is delivered once without Resume.
#[test]
fn begin_boot_old_format_applied_begin_is_delivered_once_after_restart() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.status_open_grant_state.lock().unwrap() = Some("begun".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    let sent_before = sent(&transport);
    restart(&state, &session, pre_fix);
    settle_after_restart(&state, &session);

    assert_recovery_comes_first(&transport, sent_before, "turn_evaluate");
    assert_eq!(restart_checkpoints(&transport), 0, "no restart checkpoint closes it");
    let keys = evaluate_keys(&transport);
    assert!(keys.len() >= 2, "the exact retained evaluate is replayed: {keys:?}");
    assert!(keys.iter().all(|key| key == &keys[0]), "never a new evaluate key");
    assert_every_begin_is(&transport, &first);
    assert_eq!(begins(&transport).len(), 2, "one exact begin replay");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
    assert_eq!(retained(&state, &session), 0);
}

/// Old format, expired unbegun grant. The begin never reached Engram. After
/// the restart the retained evaluate replay returns the stored grant, its
/// rebuilt begin is refused grant_expired, and the head proceeds through a
/// fresh evaluate under a new key; delivered once without Resume.
#[test]
fn begin_boot_old_format_expired_unbegun_grant_is_delivered_once_after_restart() {
    let script = ReplayScript::default();
    script.unavailable.store(1, Ordering::SeqCst);
    *script.status_open_grant_state.lock().unwrap() = Some("issued".to_owned());
    let producer = StatefulEngramControlTransport::with_first_begin_refusal("grant_expired");
    let (state, session, receiver, transport) = replay_fixture_with(producer, script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    let sent_before = sent(&transport);
    restart(&state, &session, pre_fix);
    settle_after_restart(&state, &session);

    assert_recovery_comes_first(&transport, sent_before, "turn_evaluate");
    assert_eq!(restart_checkpoints(&transport), 0);
    let keys = evaluate_keys(&transport);
    assert!(keys.len() >= 3, "replayed, then one fresh evaluate: {keys:?}");
    assert_eq!(keys[1], keys[0], "the retained evaluate is replayed exactly");
    assert_ne!(keys.last().unwrap(), &keys[0], "the fresh evaluate has a new key");
    let last = begins(&transport).last().cloned().unwrap();
    assert_ne!(last["grant_id"], first["grant_id"], "never resurrected");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
}

/// Old format, unverifiable. When the retained evaluate's replay answers with
/// another grant (the stored result was not returned), the old begin cannot
/// be rebuilt from producer authority: no begin is fabricated, nothing is
/// delivered, and the head stays held with its possibly begun grant.
#[test]
fn begin_boot_old_format_with_another_grant_on_replay_stays_held() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.status_open_grant_state.lock().unwrap() = Some("begun".to_owned());
    *script.replay_evaluate_grant.lock().unwrap() = Some("a-fresh-grant".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    restart(&state, &session, pre_fix);
    settle_after_restart(&state, &session);

    assert_eq!(begins(&transport).len(), 1, "no begin is fabricated");
    assert!(receiver.try_recv().is_err(), "nothing delivered");
    assert_eq!(retained(&state, &session), 1, "held");
    assert_eq!(uncertain(&state, &session).as_deref(), first["grant_id"].as_str());
    assert_eq!(restart_checkpoints(&transport), 0);
}

/// Old format, answered without a grant. A refusal of the retained
/// evaluate's replay (one a first evaluate would heal by rebinding) or a Defer
/// proves nothing about the begin an earlier host may have sent: no begin, no
/// fresh evaluate and no restart checkpoint; the head stays held with its
/// retained evaluate and possibly begun grant.
fn old_format_answer_without_a_grant_stays_held(reply: Value) {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.status_open_grant_state.lock().unwrap() = Some("begun".to_owned());
    *script.replay_evaluate_reply.lock().unwrap() = Some(reply);
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    restart(&state, &session, pre_fix);
    settle_after_restart(&state, &session);

    assert_eq!(begins(&transport).len(), 1, "no begin is fabricated");
    let keys = evaluate_keys(&transport);
    assert!(keys.len() >= 2, "the retained evaluate is replayed: {keys:?}");
    assert!(keys.iter().all(|key| key == &keys[0]), "no fresh evaluate: {keys:?}");
    assert!(receiver.try_recv().is_err(), "nothing delivered");
    assert_eq!(retained(&state, &session), 1, "held");
    assert_eq!(uncertain(&state, &session).as_deref(), first["grant_id"].as_str());
    assert_eq!(restart_checkpoints(&transport), 0, "no restart checkpoint closes it");
    let kept = with_record(&state, &session, |record| {
        record
            .queued_prompts
            .front()
            .is_some_and(|head| head.engram_evaluate.is_some())
    });
    assert!(kept, "the retained evaluate is kept");
}

#[test]
fn begin_boot_old_format_refused_replay_stays_held() {
    old_format_answer_without_a_grant_stays_held(json!({
        "decision": "refuse",
        "directive": {
            "directive_id": "directive-stale_fence",
            "code": "stale_fence",
            "target": "host",
            "satisfaction": "rebind"
        }
    }));
}

#[test]
fn begin_boot_old_format_deferred_replay_stays_held() {
    old_format_answer_without_a_grant_stays_held(json!({
        "decision": "defer",
        "deferral": {
            "code": "authority_unavailable",
            "retry_after_ms": 100,
            "wake_condition": "authority_available"
        }
    }));
}

/// A crash after an ordinary retry's own attempt prepared the begin: the saved
/// head carries the prepared begin beside that acknowledged ordinary retry,
/// and nothing marks its grant possibly begun. Boot still reads the durable
/// preparation as begin unknown, replaces the retry with the exact replay,
/// and the head is delivered once with no restart checkpoint.
#[test]
fn begin_boot_crash_after_preparation_beside_an_ordinary_retry_replays_the_prepared_begin() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.status_open_grant_state.lock().unwrap() = Some("begun".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    acknowledge_before_due(&state);

    let sent_before = sent(&transport);
    restart(&state, &session, |saved| {
        saved.engram_uncertain_grant_id = None;
        saved
            .engram_admission_retry
            .as_mut()
            .expect("the scheduled retry")
            .code = "control_unavailable".to_owned();
    });
    let reconstructed = with_record(&state, &session, |record| {
        (
            record.engram.uncertain_grant_id.clone(),
            record.engram.admission_retry.as_ref().map(|retry| retry.code.clone()),
        )
    });
    assert_eq!(
        reconstructed,
        (
            first["grant_id"].as_str().map(str::to_owned),
            Some(ENGRAM_BEGIN_REPLAY_CODE.to_owned())
        ),
        "the ordinary retry gives way to the exact replay"
    );
    settle_after_restart(&state, &session);

    assert_recovery_comes_first(&transport, sent_before, "turn_begin");
    assert_eq!(restart_checkpoints(&transport), 0);
    assert_every_begin_is(&transport, &first);
    assert_eq!(begins(&transport).len(), 2, "one exact begin replay");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
}

/// A crash after the prepared begin was acknowledged but before its failure
/// was recorded: the saved head carries the prepared begin and nothing marks
/// its grant possibly begun. Boot reads the durable preparation as begin
/// unknown, schedules the exact replay, and the head is delivered once.
#[test]
fn begin_boot_crash_after_preparation_replays_the_prepared_begin() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.status_open_grant_state.lock().unwrap() = Some("begun".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    let sent_before = sent(&transport);
    restart(&state, &session, |saved| {
        saved.engram_uncertain_grant_id = None;
        saved.engram_admission_retry = None;
    });
    let reconstructed = with_record(&state, &session, |record| {
        (
            record.engram.uncertain_grant_id.clone(),
            record.engram.admission_retry.as_ref().map(|retry| retry.code.clone()),
        )
    });
    assert_eq!(
        reconstructed,
        (
            first["grant_id"].as_str().map(str::to_owned),
            Some(ENGRAM_BEGIN_REPLAY_CODE.to_owned())
        ),
        "the durable preparation is read as begin unknown and scheduled"
    );
    settle_after_restart(&state, &session);

    assert_recovery_comes_first(&transport, sent_before, "turn_begin");
    assert_eq!(restart_checkpoints(&transport), 0);
    assert_every_begin_is(&transport, &first);
    assert_eq!(begins(&transport).len(), 2, "one exact begin replay");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
}
