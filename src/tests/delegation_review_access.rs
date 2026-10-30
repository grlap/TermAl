//! Same-project exact archive reads preserve identity, isolation and pure custody.
use super::*;
use super::delegation_review_attempts::terminal_review;
use super::delegation_support::{finish_delegation_child_with_assistant_text, install_delegation_codex_runtime, structured_review_request};

fn selection(id: &str) -> DelegationResultSelection {
    DelegationResultSelection { submission_attempt: Some(1), result_id: Some(id.to_owned()) }
}

fn access_fixture() -> (AppState, String, String, String, String, String) {
    let (state, parent, delegation, child) = terminal_review(&"Original validated prose. λ\n".repeat(70));
    let project_root = state.persistence_path.parent().unwrap().join("review-project");
    fs::create_dir_all(&project_root).unwrap();
    let project = create_test_project(&state, &project_root, "Review project");
    let reader = {
        let mut inner = state.inner.lock().unwrap();
        for id in [&parent, &child] {
            let index = inner.find_session_index(id).unwrap();
            inner.sessions[index].session.project_id = Some(project.clone());
        }
        let index = inner.find_delegation_index(&delegation).unwrap();
        // JSON also permits this fixture to compile against the carried archive
        // baseline, where the new commissioning field does not exist yet.
        let mut value = serde_json::to_value(&inner.delegations[index]).unwrap();
        value["originalProjectId"] = json!(project);
        inner.delegations[index] = serde_json::from_value(value).unwrap();
        let reader = inner.create_session(Agent::Codex, Some("Independent reader".into()),
            state.default_workdir.clone(), Some(project), None);
        let id = reader.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        id
    };
    let (_, id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    (state, parent, delegation, child, reader, id)
}

fn changes(state: &AppState) -> u64 {
    state.mailbox_store.connection().unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap()
}

fn revise_fixture_publication(state: &AppState, delegation: &str, child: &str,
    revise: impl FnOnce(&mut ReviewAttemptSnapshot)) -> String {
    let rows = state.review_custody_rows_read_only(Some(child), delegation).unwrap();
    let key = state.mailbox_store.review_attempt_auth_key(false).unwrap().unwrap();
    let mut slot: ReviewCustodySlot = decode_review_value(&rows[0].body, &key).unwrap();
    let mut snapshot: ReviewAttemptSnapshot = decode_review_value(
        &serde_json::to_string(slot.candidate.as_ref().unwrap()).unwrap(), &key).unwrap();
    revise(&mut snapshot);
    let candidate: SignedReviewAttempt = serde_json::from_str(&encode_review_value(&snapshot, &key).unwrap()).unwrap();
    let id = review_candidate_id(&candidate).unwrap();
    slot.candidate = Some(candidate);
    slot.publication.as_mut().unwrap().result_id = id.clone();
    state.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_messages SET body=?1 WHERE id=?2",
        rusqlite::params![encode_review_value(&slot, &key).unwrap(), rows[0].message_id],
    ).unwrap();
    id
}

#[test]
fn pinned_parent_read_never_repairs_dispatch_metadata() {
    let (state, parent, delegation, _, _, id) = access_fixture();
    state.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_messages SET dispatch_outcome=NULL WHERE topic=?1",
        rusqlite::params![REVIEW_ATTEMPT_TOPIC],
    ).unwrap();
    let before = changes(&state);
    state.get_delegation_result_selected(&parent, &delegation, &selection(&id)).unwrap();
    assert_eq!(changes(&state), before, "pinned archive read must not finalize dispatch metadata");
}

#[test]
fn eligible_root_reads_original_packet_and_pages_with_absent_parent() {
    let (state, parent, delegation, _, reader, id) = access_fixture();
    let original = state.get_delegation_result_selected(&parent, &delegation, &selection(&id)).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&parent).unwrap();
        inner.remove_session_at(index).unwrap();
    }
    let result = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).unwrap();
    assert_eq!(result.result, original.result);
    assert_eq!(result.result_id.as_deref(), Some(id.as_str()));
    assert_eq!(result.result.commands_run, original.result.commands_run);
    let mut raw = String::new();
    let mut offset = 0;
    loop {
        let page = state.get_delegation_result_output_selected(&reader, &delegation, offset, 256, &selection(&id)).unwrap();
        assert_eq!(page.result_id.as_deref(), Some(id.as_str()));
        raw.push_str(&page.output);
        if page.complete { break; }
        offset = page.next_offset_bytes.unwrap();
    }
    assert_eq!(raw, "Original validated prose. λ\n".repeat(70));
}

#[test]
fn concurrent_readers_and_returning_parent_never_finalize_or_mutate() {
    let (state, parent, delegation, _, reader, id) = access_fixture();
    state.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_messages SET dispatch_outcome=NULL WHERE topic=?1",
        rusqlite::params![REVIEW_ATTEMPT_TOPIC],
    ).unwrap();
    let before = changes(&state);
    let (revision, delegations, waits) = {
        let inner = state.inner.lock().unwrap();
        (inner.revision, serde_json::to_value(&*inner.delegations).unwrap(),
            serde_json::to_value(&*inner.delegation_waits).unwrap())
    };
    let mut jobs = Vec::new();
    for actor in [reader.clone(), parent.clone(), reader] {
        let (state, delegation, id) = (state.clone(), delegation.clone(), id.clone());
        jobs.push(std::thread::spawn(move || state.get_delegation_result_selected(&actor, &delegation, &selection(&id)).unwrap().result));
    }
    let expected = jobs.remove(0).join().unwrap();
    for job in jobs { assert_eq!(job.join().unwrap(), expected); }
    assert_eq!(changes(&state), before, "a read must not repair dispatch metadata");
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.revision, revision);
    assert_eq!(serde_json::to_value(&*inner.delegations).unwrap(), delegations);
    assert_eq!(serde_json::to_value(&*inner.delegation_waits).unwrap(), waits);
}

#[test]
fn exact_read_rejects_child_foreign_hidden_proxy_and_wrong_attempt() {
    for case in ["child", "foreign", "hidden", "proxy", "removed"] {
        let (state, _, delegation, child, reader, id) = access_fixture();
        let actor = if case == "child" { child } else { reader };
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&actor).unwrap();
            match case {
                "foreign" => inner.sessions[index].session.project_id = Some("other-project".into()),
                "hidden" => inner.sessions[index].hidden = true,
                "proxy" => { inner.sessions[index].remote_id = Some("remote".into()); inner.sessions[index].remote_session_id = Some("remote-root".into()); },
                "removed" => { inner.remove_session_at(index); },
                _ => {},
            }
        }
        assert_eq!(state.get_delegation_result_selected(&actor, &delegation, &selection(&id)).err().unwrap().status, StatusCode::NOT_FOUND, "{case}");
    }
    let (state, _, delegation, _, reader, id) = access_fixture();
    let mut wrong = selection(&id);
    wrong.submission_attempt = Some(2);
    assert_eq!(state.get_delegation_result_selected(&reader, &delegation, &wrong).err().unwrap().status, StatusCode::NOT_FOUND);
    assert!(state.get_delegation_result_selected(&reader, &delegation, &DelegationResultSelection::default()).is_err());
}

#[test]
fn legacy_missing_project_basis_is_explicit_and_never_guessed_from_cwd() {
    let (state, parent, delegation, child) = terminal_review("Legacy prose.");
    state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    let id = revise_fixture_publication(&state, &delegation, &child, |snapshot| {
        snapshot.project_basis_recorded = false;
    });
    let reader = {
        let mut inner = state.inner.lock().unwrap();
        let reader = inner.create_session(Agent::Codex, None, state.default_workdir.clone(), Some("review-project".into()), None);
        let id = reader.session.id.clone();
        let i = inner.find_session_index(&parent).unwrap();
        inner.sessions[i].session.project_id = Some("review-project".into());
        let i = inner.find_session_index(&child).unwrap();
        inner.sessions[i].session.project_id = Some("different-project".into());
        id
    };
    let error = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "project_basis_missing");
}

#[test]
fn eligible_and_parent_reads_do_not_wait_for_a_wedged_recovery_guard() {
    let (state, parent, delegation, _, reader, id) = access_fixture();
    let before = changes(&state);
    let guard = state.mailbox_store.delegation_review_recovery_guard.lock().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reading = state.clone();
    let job = std::thread::spawn(move || {
        for actor in [parent, reader] {
            let packet = reading.get_delegation_result_selected(&actor, &delegation, &selection(&id)).unwrap();
            let page = reading.get_delegation_result_output_selected(&actor, &delegation, 0, 8192, &selection(&id)).unwrap();
            sender.send((packet.result_id, page.output)).unwrap();
        }
    });
    for _ in 0..2 {
        let (result_id, output) = phase_sync::receive(&receiver, "exact read while recovery is blocked");
        assert!(result_id.is_some());
        assert_eq!(output, "Original validated prose. λ\n".repeat(70));
    }
    drop(guard);
    job.join().unwrap();
    assert_eq!(changes(&state), before);
}

#[test]
fn known_child_custody_lookup_uses_a_key_range_search() {
    let (state, _, delegation, child, _, _) = access_fixture();
    let connection = state.mailbox_store.connection().unwrap();
    let mut explain = connection.prepare(&format!("EXPLAIN QUERY PLAN {REVIEW_CUSTODY_KEYS_BY_CHILD_SQL}")).unwrap();
    let plan = explain.query_map(rusqlite::params![child, review_slot_prefix(&delegation),
        format!("{REVIEW_ATTEMPT_KEY_PREFIX}-{delegation}."), REVIEW_ATTEMPT_TOPIC],
        |row| row.get::<_, String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert!(plan.iter().any(|step| step.contains("SEARCH mailbox_messages") && step.contains("idempotency_key>")), "{plan:?}");
    assert!(!plan.iter().any(|step| step.contains("SCAN mailbox_messages")), "{plan:?}");
    assert!(!plan.iter().any(|step| step.contains("TEMP B-TREE")), "{plan:?}");
}

#[test]
fn pure_exact_read_survives_real_persistence_restart_and_deleted_delegation() {
    let (state, parent, delegation, child, reader, id) = access_fixture();
    let expected = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).unwrap().result;
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.remove_delegation_at(index);
        let index = inner.find_session_index(&parent).unwrap();
        inner.remove_session_at(index).unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.remove_session_at(index).unwrap();
        state.commit_locked(&mut inner).unwrap();
    }
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_session_index(&reader).is_some(), "the reader's project must survive persistence");
    let rebooted = AppState {
        inner: Arc::new(StateMutex::new(loaded)),
        mailbox_store: Arc::new(MailboxStore::open(&resolve_coordination_persistence_path(state.persistence_path.as_ref())).unwrap()),
        ..state.clone()
    };
    let result = rebooted.get_delegation_result_selected(&reader, &delegation, &selection(&id)).unwrap();
    assert_eq!(result.result, expected);
    assert_eq!(result.review_attempt_state.unwrap().disposition, "published_current_unavailable");
}

#[test]
fn superseded_original_is_labelled_and_invalid_publication_never_reads_clean() {
    let (state, _, delegation, child, reader, id) = access_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].review_result_submission_attempt = 2;
    }
    let result = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).unwrap();
    assert_eq!(result.review_attempt_state.unwrap().disposition, "superseded");
    state.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_messages SET body='invalid' WHERE sender_session_id=?1 AND topic=?2",
        rusqlite::params![child, REVIEW_ATTEMPT_TOPIC],
    ).unwrap();
    let error = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("history may be missing or quarantined"), "{}", error.message);
}

#[test]
fn consistent_native_facts_authorize_a_legacy_snapshot_without_project_stamp() {
    let (state, parent, delegation, child) = terminal_review("Legacy native facts.");
    let (snapshot, _) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    assert!(snapshot.original_project_id.is_none());
    let id = revise_fixture_publication(&state, &delegation, &child, |snapshot| {
        snapshot.project_basis_recorded = false; // Legacy archive predating the marker.
    });
    let reader = {
        let mut inner = state.inner.lock().unwrap();
        for actor in [&parent, &child] {
            let index = inner.find_session_index(actor).unwrap();
            inner.sessions[index].session.project_id = Some("legacy-project".into());
        }
        inner.create_session(Agent::Codex, None, state.default_workdir.clone(), Some("legacy-project".into()), None).session.id.clone()
    };
    let result = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).unwrap();
    assert_eq!(result.result, snapshot.result);
}

#[test]
fn stamped_legacy_archive_keeps_its_original_project_after_native_session_drift() {
    let (state, parent, delegation, child) = terminal_review("Original legacy-project output.");
    let (reader, foreign) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        assert!(!inner.delegations[index].project_basis_recorded);
        assert!(inner.delegations[index].original_project_id.is_none());
        for actor in [&parent, &child] {
            let index = inner.find_session_index(actor).unwrap();
            inner.sessions[index].session.project_id = Some("original-project".into());
        }
        let reader = inner.create_session(Agent::Codex, None, state.default_workdir.clone(),
            Some("original-project".into()), None).session.id.clone();
        let foreign = inner.create_session(Agent::Codex, None, state.default_workdir.clone(),
            Some("later-project".into()), None).session.id.clone();
        (reader, foreign)
    };
    let (snapshot, id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    assert!(snapshot.project_basis_recorded);
    assert_eq!(snapshot.original_project_id.as_deref(), Some("original-project"));
    {
        let mut inner = state.inner.lock().unwrap();
        for actor in [&parent, &child] {
            let index = inner.find_session_index(actor).unwrap();
            inner.sessions[index].session.project_id = Some("later-project".into());
        }
    }
    let before = changes(&state);
    let packet = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).unwrap();
    assert_eq!(packet.result, snapshot.result);
    assert_eq!(packet.review_attempt_state.unwrap().original_project_id, snapshot.original_project_id);
    let page = state.get_delegation_result_output_selected(&reader, &delegation, 0, 8192, &selection(&id)).unwrap();
    assert_eq!(page.output, "Original legacy-project output.");
    assert_eq!(state.get_delegation_result_selected(&foreign, &delegation, &selection(&id)).err().unwrap().status, StatusCode::NOT_FOUND);
    assert_eq!(state.get_delegation_result_output_selected(&foreign, &delegation, 0, 8192, &selection(&id)).err().unwrap().status, StatusCode::NOT_FOUND);
    assert_eq!(changes(&state), before);
}

#[test]
fn nonparent_original_survives_a_damaged_later_publication_with_explicit_unknown_head() {
    let (state, parent, delegation, child, reader, original_id) = access_fixture();
    let original = state.get_delegation_result_selected(&reader, &delegation, &selection(&original_id)).unwrap().result;
    finish_delegation_child_with_assistant_text(&state, &child, "Later repaired publication.");
    let (_, later_id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    assert_ne!(original_id, later_id);
    let rows = state.review_custody_rows_read_only(Some(&child), &delegation).unwrap();
    let key = state.mailbox_store.review_attempt_auth_key(false).unwrap().unwrap();
    let later = rows.iter().find(|row| {
        let slot: ReviewCustodySlot = decode_review_value(&row.body, &key).unwrap();
        slot.publication.as_ref().is_some_and(|proof| proof.result_id == later_id)
    }).unwrap();
    state.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_messages SET body='damaged later publication' WHERE id=?1", [&later.message_id]).unwrap();
    let stored = state.review_custody_rows_read_only(Some(&child), &delegation).unwrap();
    let before = changes(&state);
    let packet = state.get_delegation_result_selected(&reader, &delegation, &selection(&original_id)).unwrap();
    assert_eq!(packet.result, original);
    assert_eq!(packet.review_attempt_state.unwrap().disposition, "published_head_unavailable");
    let page = state.get_delegation_result_output_selected(&reader, &delegation, 0, 8192, &selection(&original_id)).unwrap();
    assert_eq!(page.output, "Original validated prose. λ\n".repeat(70));
    assert_eq!(page.review_attempt_state.unwrap().disposition, "published_head_unavailable");
    let after = state.review_custody_rows_read_only(Some(&child), &delegation).unwrap();
    assert_eq!(after.iter().map(|row| (&row.message_id, &row.body)).collect::<Vec<_>>(),
        stored.iter().map(|row| (&row.message_id, &row.body)).collect::<Vec<_>>());
    assert_eq!(changes(&state), before);
}

#[tokio::test]
async fn eligible_nonparent_http_handlers_parse_pinned_packet_and_page_queries() {
    let (state, _, delegation, _, reader, id) = access_fixture();
    let uri = format!("/result?submissionAttempt=1&resultId={id}").parse::<axum::http::Uri>().unwrap();
    let query = Query::<DelegationResultSelection>::try_from_uri(&uri).unwrap();
    let Json(packet) = get_delegation_result(AxumPath((reader.clone(), delegation.clone())), State(state.clone()), Ok(query)).await.unwrap();
    assert_eq!(packet.result_id.as_deref(), Some(id.as_str()));
    let uri = format!("/result/output?submissionAttempt=1&resultId={id}&offsetBytes=0&limitBytes=256").parse::<axum::http::Uri>().unwrap();
    let query = Query::<DelegationResultOutputQuery>::try_from_uri(&uri).unwrap();
    let Json(page) = get_delegation_result_output(AxumPath((reader, delegation)), State(state), Ok(query)).await.unwrap();
    assert_eq!(page.result_id, packet.result_id);
    assert_eq!(page.offset_bytes, 0);
    assert!(!page.summary_fallback);
}

#[test]
fn unpromoted_candidate_is_pending_without_recovery_or_publication() {
    let (state, _, delegation, child, reader, id) = access_fixture();
    let rows = state.review_custody_rows_read_only(Some(&child), &delegation).unwrap();
    let key = state.mailbox_store.review_attempt_auth_key(false).unwrap().unwrap();
    let mut slot: ReviewCustodySlot = decode_review_value(&rows[0].body, &key).unwrap();
    slot.publication = None;
    let body = encode_review_value(&slot, &key).unwrap();
    state.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_messages SET body=?1 WHERE id=?2", rusqlite::params![body, rows[0].message_id],
    ).unwrap();
    let before = changes(&state);
    let error = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).err().unwrap();
    assert!(error.message.starts_with("review_result_pending:"), "{}", error.message);
    assert_eq!(changes(&state), before);
}

#[test]
fn earlier_publication_of_the_current_attempt_is_explicitly_superseded() {
    let (state, parent, delegation, child, reader, first_id) = access_fixture();
    let original = state.get_delegation_result_selected(&reader, &delegation, &selection(&first_id)).unwrap().result;
    finish_delegation_child_with_assistant_text(&state, &child, "A repaired terminal cut in the same attempt.");
    let (_, latest_id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    assert_ne!(latest_id, first_id);
    for actor in [&parent, &reader] {
        let old = state.get_delegation_result_selected(actor, &delegation, &selection(&first_id)).unwrap();
        assert_eq!(old.result, original);
        let observed = old.review_attempt_state.unwrap();
        assert_eq!(observed.current_submission_attempt, Some(1));
        assert_eq!(observed.disposition, "superseded");
        let latest = state.get_delegation_result_selected(actor, &delegation, &selection(&latest_id)).unwrap();
        assert_eq!(latest.review_attempt_state.unwrap().disposition, "published");
        let old_page = state.get_delegation_result_output_selected(actor, &delegation, 0, 8192, &selection(&first_id)).unwrap();
        assert_eq!(old_page.review_attempt_state.unwrap().disposition, "superseded");
        assert_eq!(old_page.output, "Original validated prose. λ\n".repeat(70));
    }
}

#[test]
fn recorded_no_project_cannot_gain_authority_from_later_native_project_changes() {
    let (state, parent, delegation, child) = terminal_review("Commissioned without a project.");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].project_basis_recorded = true;
        inner.delegations[index].original_project_id = None;
    }
    let (snapshot, id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    assert!(snapshot.project_basis_recorded);
    assert!(snapshot.original_project_id.is_none());
    let reader = {
        let mut inner = state.inner.lock().unwrap();
        for actor in [&parent, &child] {
            let index = inner.find_session_index(actor).unwrap();
            inner.sessions[index].session.project_id = Some("later-project".into());
        }
        let reader = inner.create_session(Agent::Codex, None, state.default_workdir.clone(), Some("later-project".into()), None).session.id.clone();
        let index = inner.find_delegation_index(&delegation).unwrap();
        assert_eq!(serde_json::to_value(&inner.delegations[index]).unwrap()["projectBasisRecorded"], true);
        assert!(review_original_project_locked(&inner, &inner.delegations[index]).is_none());
        reader
    };
    for removed in [false, true] {
        if removed {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&delegation).unwrap();
            inner.remove_delegation_at(index);
        }
        let error = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).err().unwrap();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.message, "project_basis_missing");
    }
}

#[test]
fn archived_later_attempt_stays_superseding_after_parent_deletion_and_restart() {
    let (state, parent, delegation, child, reader, first_id) = access_fixture();
    let original = state.get_delegation_result_selected(&reader, &delegation, &selection(&first_id)).unwrap().result;
    install_delegation_codex_runtime(&state, "access-later-attempt");
    state.followup_delegation(&parent, &delegation, "Next review attempt.".into()).unwrap();
    state.submit_delegation_review_result(&child, structured_review_request()).unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "Second attempt original output.");
    state.get_delegation_result(&parent, &delegation).unwrap();
    state.retain_review_attempt(&parent, &delegation, 2).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.remove_delegation_at(index);
        let index = inner.find_session_index(&parent).unwrap();
        inner.remove_session_at(index).unwrap();
        state.commit_locked(&mut inner).unwrap();
    }
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_session_index(&reader).is_some(), "the reader's project must survive persistence");
    let rebooted = AppState { inner: Arc::new(StateMutex::new(loaded)),
        mailbox_store: Arc::new(MailboxStore::open(&resolve_coordination_persistence_path(state.persistence_path.as_ref())).unwrap()),
        ..state.clone() };
    let packet = rebooted.get_delegation_result_selected(&reader, &delegation, &selection(&first_id)).unwrap();
    assert_eq!(packet.result, original);
    assert_eq!(packet.review_attempt_state.unwrap().disposition, "superseded");
    let page = rebooted.get_delegation_result_output_selected(&reader, &delegation, 0, 8192, &selection(&first_id)).unwrap();
    assert_eq!(page.review_attempt_state.unwrap().disposition, "superseded");
    assert_eq!(page.output, "Original validated prose. λ\n".repeat(70));
}

#[test]
fn nonparent_error_branches_keep_exact_conflict_status_and_reason() {
    for case in ["unvalidated", "empty-pending", "empty-basis"] {
        let (state, _, delegation, child, reader, mut id) = access_fixture();
        let expected = match case {
            "unvalidated" => {
                id = revise_fixture_publication(&state, &delegation, &child, |snapshot| {
                    snapshot.provenance.submission = "corrupt".into();
                });
                "review_result_unvalidated:"
            },
            _ => {
                state.mailbox_store.connection().unwrap().execute(
                    "DELETE FROM mailbox_messages WHERE topic=?1", rusqlite::params![REVIEW_ATTEMPT_TOPIC]).unwrap();
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_delegation_index(&delegation).unwrap();
                inner.delegations[index].status = DelegationStatus::Running;
                if case == "empty-basis" {
                    inner.delegations[index].original_project_id = None;
                    inner.delegations[index].project_basis_recorded = true;
                    "project_basis_missing"
                } else { "review_result_pending:" }
            },
        };
        let error = state.get_delegation_result_selected(&reader, &delegation, &selection(&id)).err().unwrap();
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}");
        assert!(error.message.starts_with(expected), "{case}: {}", error.message);
    }
}
