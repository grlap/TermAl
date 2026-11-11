// Evaluation measurements meet the real ordinary turn-close/report owner.
// The source owner and observation transport are fixtures, not live stores.
use super::*;

fn wait_for_closing_reports(turn: &EOCheckedTurn) {
    let guard = crate::tests::phase_sync::PollGuard::new();
    while !turn.record(|record| {
        !record.engram.unadmitted_worker_running
            && record.engram.unadmitted_closing_jobs.is_empty()
            && record
                .engram
                .unadmitted_reports
                .iter()
                .all(EngramRuntimeObservationReport::is_settled)
    }) {
        guard.wait("the fixture's closing reports settle");
    }
}

// Step the production background delta selection and SQLite writer. An
// endpoint-only update must be selected after an earlier watermark drained.
struct EndpointDeltaWriter {
    rx: mpsc::Receiver<PersistRequest>,
    inner: Arc<StateMutex<StateInner>>,
    cache: SqlitePersistConnectionCache,
    path: PathBuf,
    batch: PersistFenceBatch,
    watermark: u64,
}

impl EndpointDeltaWriter {
    fn attach(state: &mut AppState) -> Self {
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx;
        Self {
            rx,
            inner: state.inner.clone(),
            cache: SqlitePersistConnectionCache::new(),
            path: state.persistence_path.as_ref().clone(),
            batch: PersistFenceBatch::default(),
            watermark: 0,
        }
    }

    fn write(&mut self) -> Vec<String> {
        for request in self.rx.try_iter() {
            self.batch.accept(request);
        }
        let delta = collect_persist_delta_from_shared_state(&self.inner, self.watermark);
        let selected = delta
            .changed_sessions
            .iter()
            .map(|record| record.session.id.clone())
            .collect();
        persist_delta_with_fences(&mut self.cache, &self.path, &delta, &mut self.batch).unwrap();
        self.watermark = delta.watermark;
        selected
    }
}

fn evaluated_source(turn: &EOCheckedTurn) -> EngramSourceId {
    let place = turn.record(|record| record.engram.observed_source_root().unwrap().place());
    let (basis, _, _) = turn
        .state
        .acceptance_owner_judged_source(&turn.session_id, &place, None)
        .expect("the evaluation reads the confirmed named source");
    let Some(AcceptanceSourceBasis::Shared(basis)) = basis else {
        panic!("the evaluation must judge a complete measured identity");
    };
    basis.source_id
}

fn edit_named_source(turn: &EOCheckedTurn) {
    let root =
        turn.record(|record| PathBuf::from(&record.engram.observed_source_root().unwrap().root));
    fs::write(root.join("README.md"), "changed after evaluation\n").unwrap();
    turn.sources.advance(&root, "README.md");
}

#[test]
fn unchanged_evaluated_source_does_not_become_an_assumed_change_at_turn_end() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::start_named("evaluation-quiet-close");
    assert!(turn.record(|record| record.engram.runtime_checkpoint.is_none()));
    let judged = evaluated_source(&turn);
    let observation = turn.finish();
    assert!(
        observation["occurrence"]["source_change"].is_null(),
        "ending an unchanged evaluated turn must not report a source-change barrier: {observation}"
    );
    // The independently measured closing sighting must still be sent. Quiet
    // change reporting cannot manufacture an unmeasured equality assertion.
    let record = turn.record(|record| record.engram.unadmitted_reports.clone());
    let sighting = record
        .iter()
        .flat_map(|report| &report.envelopes)
        .find(|envelope| {
            envelope.request["occurrence"]["kind"] == "source_sighting"
                && envelope.request["occurrence"]["source_basis"]["source_id"]
                    == serde_json::to_value(&judged).unwrap()
        });
    assert!(
        sighting.is_some(),
        "the real closing source measurement remains reported"
    );
}

#[test]
fn an_edit_after_evaluation_is_reported_against_the_judged_source() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::start_named("evaluation-edited-close");
    let judged = evaluated_source(&turn);
    edit_named_source(&turn);
    let observation = turn.finish();
    let change = &observation["occurrence"]["source_change"];
    assert_eq!(change["detection"], "content_comparison");
    assert_eq!(
        change["baseline"]["source_id"],
        serde_json::to_value(judged).unwrap()
    );
    assert_ne!(
        change["baseline"]["source_id"],
        change["sighting"]["source_basis"]["source_id"]
    );
}

#[test]
fn an_evaluation_never_replaces_an_existing_measured_change_baseline() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::start_named("evaluation-keeps-earlier-change");
    let place = turn.record(|record| record.engram.observed_source_root().unwrap().place());
    let before = turn
        .state
        .engram_root_capture(&turn.session_id)
        .stamp(Some(turn.capture_basis(&place)))
        .expect("the prior measurement belongs to the confirmed named root");
    assert_eq!(before.source_root_state, Some(EngramSourceRootState::Named));
    assert_eq!(
        before.source_root_generation,
        turn.record(|record| Some(record.engram.observed_source_root().unwrap().generation as i64))
    );
    turn.record_mut(|record| {
        record.engram.runtime_checkpoint = Some(EngramRuntimeCheckpointBasis::of_sighting(
            record.engram.work_binding.clone().unwrap(),
            &before,
            &unadmitted_observation_timestamp(),
        ));
    });
    edit_named_source(&turn);
    let judged = evaluated_source(&turn);
    let observation = turn.finish();
    let change = &observation["occurrence"]["source_change"];
    assert_eq!(change["detection"], "content_comparison");
    assert_eq!(
        change["baseline"]["source_id"],
        serde_json::to_value(before.source_id.unwrap()).unwrap()
    );
    assert_eq!(
        change["sighting"]["source_basis"]["source_id"],
        serde_json::to_value(judged).unwrap()
    );
    assert_ne!(
        change["baseline"]["source_id"],
        change["sighting"]["source_basis"]["source_id"]
    );
}

#[test]
fn the_request_endpoint_survives_sqlite_reload_but_cannot_bridge_a_later_missing_checkpoint() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::start_named("evaluation-later-quiet-turn");
    let judged = evaluated_source(&turn);
    let original = turn.record(|record| record.engram.evaluation_checkpoint.clone().unwrap());
    // Read the real fixture SQLite store, not just a serde round trip.
    {
        let inner = turn.state.inner.lock().unwrap();
        persist_state(turn.state.persistence_path.as_ref(), &inner).unwrap();
    }
    let restored = load_state(turn.state.persistence_path.as_ref())
        .unwrap()
        .unwrap();
    let restored = restored.sessions[restored.find_session_index(&turn.session_id).unwrap()]
        .engram
        .evaluation_checkpoint
        .clone()
        .unwrap();
    assert_eq!(
        restored, original,
        "reload preserves the original measurement and time"
    );
    assert!(turn.finish()["occurrence"]["source_change"].is_null());
    wait_for_closing_reports(&turn);
    assert!(turn.record(|record| record.engram.evaluation_checkpoint.is_none()));

    turn.begin("Read the evaluator result.");
    // A predecessor's closing sighting may not have established an ordinary
    // checkpoint before a successor starts. Exercise that missing endpoint.
    turn.record_mut(|record| {
        record.engram.runtime_checkpoint = None;
    });
    assert_eq!(
        turn.finish()["occurrence"]["source_change"]["detection"],
        "assumed_missing_baseline"
    );
    wait_for_closing_reports(&turn);

    turn.begin("Edit after the quiet follow-up.");
    turn.record_mut(|record| record.engram.runtime_checkpoint = None);
    assert_eq!(
        evaluated_source(&turn),
        judged,
        "a new request really remeasures X"
    );
    let renewed = turn.record(|record| record.engram.evaluation_checkpoint.clone().unwrap());
    edit_named_source(&turn);
    let observation = turn.finish();
    let change = &observation["occurrence"]["source_change"];
    assert_eq!(change["detection"], "content_comparison");
    assert_eq!(
        change["baseline"]["source_id"],
        serde_json::to_value(judged).unwrap()
    );
    assert_eq!(
        change["baseline"]["observed_at"],
        renewed.checkpoint.endpoint.observed_at
    );
    assert_ne!(
        change["baseline"]["source_id"],
        change["sighting"]["source_basis"]["source_id"]
    );
}

#[test]
fn unavailable_unadmitted_or_other_root_measurements_do_not_supply_a_checkpoint() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for fault in ["unavailable", "admission", "other-root", "no-evaluation"] {
        let turn = EOCheckedTurn::start_named(&format!("evaluation-no-checkpoint-{fault}"));
        let original = turn
            .state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner
            .clone()
            .unwrap();
        let place = turn.record(|record| record.engram.observed_source_root().unwrap().place());
        if fault == "unavailable" {
            let version = original.clone();
            turn.state
                .inner
                .lock()
                .unwrap()
                .test_observation_source_owner =
                Some(Arc::new(move |binary, cwd, args, deadline, limit| {
                    if args[0] == "--version" {
                        return version(binary, cwd, args, deadline, limit);
                    }
                    Ok(crate::tests::engram_source_owner::source_owner_output(
                        1,
                        serde_json::to_vec(&crate::tests::engram_source_command::source_envelope(
                            FsPath::new(&args[2]),
                            "unavailable",
                        ))
                        .unwrap(),
                    ))
                }));
            let (basis, _, _) = turn
                .state
                .acceptance_owner_judged_source(&turn.session_id, &place, None)
                .unwrap();
            assert!(basis.is_none());
        } else if fault == "admission" {
            let admission = AcceptanceCommittedInputAdmission {
                run_id: "test-fixture".into(),
                commit: "f".repeat(40),
                native_source: None,
            };
            assert!(
                turn.state
                    .acceptance_owner_judged_source(&turn.session_id, &place, Some(&admission))
                    .is_err()
            );
        } else if fault == "other-root" {
            let other = EngramBasisPlace::Workdir(turn.root.to_string_lossy().into_owned());
            let (basis, _, _) = turn
                .state
                .acceptance_owner_judged_source(&turn.session_id, &other, None)
                .unwrap();
            assert!(
                basis.is_some(),
                "a real measurement of a different root is not this turn's baseline"
            );
        }
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner = Some(original);
        assert!(
            turn.record(|record| record.engram.evaluation_checkpoint.is_none()),
            "fault={fault}"
        );
        assert_eq!(
            turn.finish()["occurrence"]["source_change"]["detection"],
            "assumed_missing_baseline",
            "fault={fault}"
        );
    }
}

#[test]
fn evaluation_capture_rechecks_its_original_binding_trace_and_named_root() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for fault in ["none", "binding", "trace", "generation", "uncertain"] {
        let turn = EOCheckedTurn::start_named(&format!("evaluation-capture-race-{fault}"));
        let original = turn
            .state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner
            .clone()
            .unwrap();
        let run = original.clone();
        let weak = Arc::downgrade(&turn.state.inner);
        let owner = turn.session_id.clone();
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let once = fired.clone();
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner =
            Some(Arc::new(move |binary, cwd, args, deadline, limit| {
                let result = run(binary, cwd, args, deadline, limit);
                if args[0] != "--version" && !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let inner = weak.upgrade().unwrap();
                    let mut inner = inner.lock().unwrap();
                    let index = inner.find_session_index(&owner).unwrap();
                    match fault {
                        "none" => {}
                        "binding" => {
                            inner.sessions[index]
                                .engram
                                .work_binding
                                .as_mut()
                                .unwrap()
                                .work_revision += 1
                        }
                        "trace" => inner.sessions[index]
                            .engram
                            .unadmitted_active_turn
                            .as_mut()
                            .unwrap()
                            .host_turn_ref
                            .push_str("-replaced"),
                        "generation" => inner.engram_work_source_roots[0].generation += 1,
                        "uncertain" => {
                            inner.sessions[index]
                                .engram
                                .unadmitted_active_turn
                                .as_mut()
                                .unwrap()
                                .source_attribution_uncertain = true
                        }
                        _ => unreachable!(),
                    }
                }
                result
            }));
        let judged = evaluated_source(&turn);
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner = Some(original);
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            !judged.commit.is_empty(),
            "the owner really measured a source"
        );
        assert_eq!(
            turn.record(|record| record.engram.evaluation_checkpoint.is_some()),
            fault == "none",
            "fault={fault}"
        );
    }
}

#[test]
fn a_retained_evaluation_endpoint_cannot_cross_store_binding_or_root_images() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for fault in [
        "none",
        "store",
        "binding",
        "workspace",
        "generation",
        "state",
        "turn",
        "turn-generation",
    ] {
        let turn = EOCheckedTurn::start_named(&format!("evaluation-retained-scope-{fault}"));
        evaluated_source(&turn);
        turn.record_mut(|record| {
            assert!(record.engram.runtime_checkpoint.is_none());
            let retained = record.engram.evaluation_checkpoint.as_mut().unwrap();
            match fault {
                "none" => {}
                "store" => {
                    retained.store.database_path.set_extension("another-store");
                }
                "binding" => retained.checkpoint.binding.work_revision += 1,
                "workspace" => retained
                    .checkpoint
                    .endpoint
                    .workspace_id
                    .push_str("-another-root"),
                "generation" => retained.checkpoint.source_root_generation = Some(999),
                "state" => {
                    retained.checkpoint.source_root_state = Some(EngramSourceRootState::Ended)
                }
                "turn" => retained.host_turn_ref.push_str("-another-turn"),
                "turn-generation" => retained.generation += 1,
                _ => unreachable!(),
            }
        });
        {
            let inner = turn.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
            let scope = EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap();
            let guarded = evaluation_closing_checkpoint(
                record.engram.evaluation_checkpoint.as_ref(),
                record.engram.unadmitted_active_turn.as_ref().unwrap(),
                &scope,
            );
            assert_eq!(
                guarded.is_some(),
                fault == "none",
                "direct guard, fault={fault}"
            );
        }
        let observation = turn.finish();
        if fault == "none" {
            assert!(observation["occurrence"]["source_change"].is_null());
        } else {
            assert_eq!(
                observation["occurrence"]["source_change"]["detection"], "assumed_missing_baseline",
                "fault={fault}"
            );
        }
    }
}

#[test]
fn an_endpoint_only_update_is_written_by_the_drained_background_delta_path() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut turn = EOCheckedTurn::start_named("evaluation-endpoint-delta");
    let mut writer = EndpointDeltaWriter::attach(&mut turn.state);
    assert!(writer.write().contains(&turn.session_id));
    assert!(
        writer.write().is_empty(),
        "the old session image is fully drained"
    );
    let before = turn.record(|record| record.mutation_stamp);
    evaluated_source(&turn);
    assert!(
        turn.record(|record| record.mutation_stamp) > before,
        "the endpoint is the only session mutation"
    );
    let expected = turn.record(|record| record.engram.evaluation_checkpoint.clone().unwrap());
    assert_eq!(writer.write(), vec![turn.session_id.clone()]);
    assert!(writer.write().is_empty());
    let disk = load_state(turn.state.persistence_path.as_ref())
        .unwrap()
        .unwrap();
    assert_eq!(
        disk.sessions[disk.find_session_index(&turn.session_id).unwrap()]
            .engram
            .evaluation_checkpoint
            .as_ref(),
        Some(&expected)
    );
}

#[test]
fn an_intervening_changed_or_unmeasured_close_prevents_cross_turn_x_y_x_equality() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for mode in ["changed", "unavailable"] {
        let turn = EOCheckedTurn::start_named(&format!("evaluation-cross-turn-revert-{mode}"));
        let root = turn
            .record(|record| PathBuf::from(&record.engram.observed_source_root().unwrap().root));
        let contents_x = fs::read(root.join("README.md")).unwrap();
        let x = evaluated_source(&turn);
        assert!(turn.finish()["occurrence"]["source_change"].is_null());
        wait_for_closing_reports(&turn);
        assert!(turn.record(|record| record.engram.runtime_checkpoint.is_some()));

        turn.begin("Make the intervening change Y.");
        edit_named_source(&turn);
        assert_ne!(turn.sources.get(&root), x);
        let turn_y = turn.record(|record| {
            record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .host_turn_ref
                .clone()
        });
        let original = turn
            .state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner
            .clone()
            .unwrap();
        if mode == "changed" {
            // Freeze the actual Y owner reply until turn 3 starts. The later
            // ACK then cannot establish an ordinary checkpoint for turn 2.
            let run = original.clone();
            let (seen, captured) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let wait = Arc::new(Mutex::new(wait));
            let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
            turn.state
                .inner
                .lock()
                .unwrap()
                .test_observation_source_owner =
                Some(Arc::new(move |binary, cwd, args, deadline, limit| {
                    let result = run(binary, cwd, args, deadline, limit);
                    if args[0] != "--version"
                        && !once.swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        seen.send(()).unwrap();
                        wait.lock().unwrap().recv_timeout(DEADLOCK_GUARD).unwrap();
                    }
                    result
                }));
            turn.end();
            captured.recv_timeout(DEADLOCK_GUARD).unwrap();
            turn.begin("Start before Y's closing sighting settles.");
            release.send(()).unwrap();
            let changed = turn.observation_for(&turn_y);
            assert_eq!(
                changed["occurrence"]["source_change"]["detection"],
                "content_comparison"
            );
            assert_ne!(
                changed["occurrence"]["source_change"]["sighting"]["source_basis"]["source_id"],
                serde_json::to_value(&x).unwrap()
            );
            wait_for_closing_reports(&turn);
        } else {
            let version = original.clone();
            turn.state
                .inner
                .lock()
                .unwrap()
                .test_observation_source_owner =
                Some(Arc::new(move |binary, cwd, args, deadline, limit| {
                    if args[0] == "--version" {
                        return version(binary, cwd, args, deadline, limit);
                    }
                    Ok(crate::tests::engram_source_owner::source_owner_output(
                        1,
                        serde_json::to_vec(&crate::tests::engram_source_command::source_envelope(
                            FsPath::new(&args[2]),
                            "unavailable",
                        ))
                        .unwrap(),
                    ))
                }));
            assert!(turn.finish()["occurrence"]["source_change"].is_null());
            wait_for_closing_reports(&turn);
            turn.begin("Resume after an unmeasured Y close.");
        }
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner = Some(original);
        assert!(
            turn.record(|record| record.engram.runtime_checkpoint.is_none()),
            "mode={mode}"
        );
        fs::write(root.join("README.md"), &contents_x).unwrap();
        turn.sources.set(&root, x.clone());
        let reverted = turn.finish();
        let change = &reverted["occurrence"]["source_change"];
        assert_eq!(
            change["detection"], "assumed_missing_baseline",
            "mode={mode}"
        );
        assert_eq!(
            change["sighting"]["source_basis"]["source_id"],
            serde_json::to_value(&x).unwrap()
        );
    }
}
