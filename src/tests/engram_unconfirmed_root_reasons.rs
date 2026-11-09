//! An unconfirmed source root names the check that failed, in the capture and
//! in the turn-opening notice, and never changes what is confirmed: every case
//! also asserts the unexplained capture agrees. Mounted from
//! `engram_source_root_naming.rs` to share its naming fixtures.
use super::*;

type Explained = std::result::Result<EngramRootCapture, EngramUnconfirmedRootReason>;

struct Named {
    claimed: EOCheckedTurn,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    generation: u64,
    worktree: PathBuf,
}

fn named(label: &str) -> Named {
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    let generation = request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)])
        .unwrap()
        .generation;
    let store = naming_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    Named {
        claimed,
        store,
        binding,
        generation,
        worktree,
    }
}

/// The capture after `mutate`, with the root state restored afterwards, so
/// each case starts from the same confirmed name.
fn explained_after(fixture: &Named, mutate: impl FnOnce(&mut StateInner, usize)) -> Explained {
    let mut inner = fixture.claimed.state.inner.lock().unwrap();
    let index = inner
        .find_session_index(&fixture.claimed.session_id)
        .unwrap();
    let journal = (*inner.engram_named_root_journal).clone();
    let history = (*inner.engram_work_naming_history).clone();
    let roots = (*inner.engram_work_source_roots).clone();
    let named_root = inner.sessions[index].engram.named_root.clone();
    let turn = inner.sessions[index].engram.unadmitted_active_turn.clone();
    mutate(&mut inner, index);
    let explained = engram_turn_root_capture_explained_locked(&inner, &fixture.claimed.session_id);
    assert_eq!(
        explained.clone().unwrap_or(EngramRootCapture::Unconfirmed),
        engram_turn_root_capture_locked(&inner, &fixture.claimed.session_id),
        "the reason never changes what is confirmed"
    );
    *inner.engram_named_root_journal = journal;
    *inner.engram_work_naming_history = history;
    *inner.engram_work_source_roots = roots;
    inner.sessions[index].engram.named_root = named_root;
    inner.sessions[index].engram.unadmitted_active_turn = turn;
    explained
}

fn history_of<'a>(inner: &'a mut StateInner, fixture: &Named) -> &'a mut EngramWorkNamingHistory {
    inner
        .engram_work_naming_history
        .iter_mut()
        .find(|history| {
            history.store == fixture.store && history.work_id == fixture.binding.work_id
        })
        .expect("the named work has naming history")
}

fn journal_of<'a>(inner: &'a mut StateInner, fixture: &Named) -> &'a mut EngramNamedRootJournal {
    inner
        .engram_named_root_journal
        .iter_mut()
        .find(|journal| {
            journal.store == fixture.store && journal.claim_id == fixture.binding.claim_id
        })
        .expect("the named claim has a root journal")
}

#[test]
fn each_unconfirmed_capture_names_the_check_that_failed() {
    use EngramUnconfirmedRootReason as Reason;
    let fixture = named("unconfirmed-reasons");
    assert!(matches!(
        explained_after(&fixture, |_, _| {}),
        Ok(EngramRootCapture::Recorded {
            state: EngramSourceRootState::Named,
            ..
        })
    ));

    let cases: Vec<(Reason, Box<dyn Fn(&mut StateInner, usize, &Named)>)> = vec![
        (
            Reason::WorkHistoryMissing,
            Box::new(|inner, _, f| {
                let (store, work) = (f.store.clone(), f.binding.work_id.clone());
                inner
                    .engram_work_naming_history
                    .retain(|h| !(h.store == store && h.work_id == work));
            }),
        ),
        (
            Reason::WorkHistoryEpoch,
            Box::new(|inner, _, f| {
                history_of(inner, f).epoch = ENGRAM_NAMING_HISTORY_EPOCH + 1;
            }),
        ),
        (
            Reason::WorkProofMissing,
            Box::new(|inner, _, f| history_of(inner, f).proofs.clear()),
        ),
        (
            Reason::WorkRunsUnresolved,
            Box::new(|inner, _, f| {
                history_of(inner, f).unresolved_runs.push(f.binding.clone());
            }),
        ),
        (
            Reason::WorkTransitionUnpublished,
            Box::new(|inner, _, f| {
                history_of(inner, f).transition = None;
            }),
        ),
        (
            Reason::WorkJournalReconciliation,
            Box::new(|inner, _, f| {
                let journal = journal_of(inner, f);
                journal.pending = journal.confirmed.as_ref().map(|(event, _)| event.clone());
            }),
        ),
        (
            Reason::ClaimProofMissing,
            Box::new(|inner, _, f| {
                for proof in &mut history_of(inner, f).proofs {
                    proof.binding.run_id = "another-run".to_owned();
                }
            }),
        ),
        (
            Reason::NamedRootUnknown,
            Box::new(|inner, index, _| {
                inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Unknown);
            }),
        ),
        (
            Reason::BoundRootMismatch,
            Box::new(|inner, index, f| {
                let Some(EngramNamedRootState::Bound {
                    workspace_id,
                    named_at,
                    ..
                }) = inner.sessions[index].engram.named_root.clone()
                else {
                    panic!("named root is bound")
                };
                inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Bound {
                    workspace_id,
                    named_at,
                    generation: f.generation as i64 + 1,
                });
            }),
        ),
        (
            Reason::ConfirmedEventRetired,
            Box::new(|inner, index, f| {
                inner.sessions[index].engram.named_root = None;
                let journal = journal_of(inner, f);
                journal.obsolete = true;
                journal.retirement = Some(EngramRootRetirement::Unbound);
            }),
        ),
        (
            Reason::LocalEntryWithoutEvent,
            Box::new(|inner, index, f| {
                inner.sessions[index].engram.named_root = None;
                journal_of(inner, f).confirmed = None;
            }),
        ),
        (
            Reason::OpeningWithoutRoot,
            Box::new(|inner, index, _| {
                let record = &mut inner.sessions[index];
                let mut trace = EngramRuntimeObservedTurn::started(
                    "reason-prompt".into(),
                    record.active_turn_generation,
                );
                trace.opening_prepared = true;
                record.engram.unadmitted_active_turn = Some(trace);
            }),
        ),
        (
            Reason::OpeningCapturedUnconfirmed,
            Box::new(|inner, index, f| {
                let record = &mut inner.sessions[index];
                let mut trace = EngramRuntimeObservedTurn::started(
                    "reason-prompt".into(),
                    record.active_turn_generation,
                );
                trace.opening_prepared = true;
                trace.opening_root = Some(EngramRuntimeOpeningRoot {
                    provenance: EngramRootCapture::Unconfirmed,
                    store: f.store.clone(),
                    binding: f.binding.clone(),
                    source_root: None,
                });
                record.engram.unadmitted_active_turn = Some(trace);
            }),
        ),
    ];
    for (reason, mutate) in &cases {
        assert_eq!(
            explained_after(&fixture, |inner, index| mutate(inner, index, &fixture)),
            Err(*reason),
            "{reason:?}"
        );
    }

    {
        let inner = fixture.claimed.state.inner.lock().unwrap();
        assert_eq!(
            engram_root_capture_explained_locked(&inner, "no-such-session"),
            Err(Reason::SessionMissing)
        );
    }
    change_engram_settings(&fixture.claimed.state, |settings| {
        settings.authority_store_key = None
    });
    assert_eq!(
        explained_after(&fixture, |_, _| {}),
        Err(Reason::StoreIdentityMissing)
    );
    change_engram_settings(&fixture.claimed.state, |settings| {
        settings.authority_store_key = Some(fixture.store.clone())
    });
    assert!(
        matches!(
            explained_after(&fixture, |_, _| {}),
            Ok(EngramRootCapture::Recorded { .. })
        ),
        "every case restored the confirmed name"
    );
}

#[test]
fn a_journal_names_exactly_the_reconciliation_it_requires() {
    use EngramUnconfirmedRootReason as Reason;
    let fixture = named("unconfirmed-journal-reasons");
    let mut journal = {
        let mut inner = fixture.claimed.state.inner.lock().unwrap();
        journal_of(&mut inner, &fixture).clone()
    };
    let event = journal
        .confirmed
        .as_ref()
        .map(|(event, _)| event.clone())
        .unwrap();
    let mut variants: Vec<(Option<Reason>, EngramNamedRootJournal)> = vec![(None, journal.clone())];
    journal.pending = Some(event.clone());
    variants.push((Some(Reason::JournalPending), journal.clone()));
    journal.pending = None;
    journal.reconciliation = Some(EngramRootReconciliation {
        intent: event,
        state: EngramNamedRootState::None,
        observed_by: "fixture".into(),
        observed_at: String::new(),
        settled: false,
    });
    variants.push((
        Some(Reason::JournalReconciliationUnsettled),
        journal.clone(),
    ));
    journal.reconciliation.as_mut().unwrap().settled = true;
    variants.push((None, journal.clone()));
    journal.obsolete = true;
    journal.retirement = None;
    variants.push((Some(Reason::JournalRetired), journal.clone()));
    for retirement in [
        EngramRootRetirement::Unbound,
        EngramRootRetirement::Displaced,
    ] {
        journal.retirement = Some(retirement);
        variants.push((None, journal.clone()));
    }
    for (expected, journal) in variants {
        assert_eq!(EngramUnconfirmedRootReason::of_journal(&journal), expected);
        assert_eq!(
            expected.is_some(),
            journal.requires_reconciliation(),
            "the reason names exactly the journals that require reconciliation"
        );
    }
}

#[test]
fn an_unconfirmed_opening_notice_names_its_reason() {
    let fixture = named("unconfirmed-opening-reason");
    let (named_root, confirmed) = {
        let mut inner = fixture.claimed.state.inner.lock().unwrap();
        let index = inner
            .find_session_index(&fixture.claimed.session_id)
            .unwrap();
        let named_root = inner.sessions[index].engram.named_root.take();
        let confirmed = journal_of(&mut inner, &fixture).confirmed.take();
        (named_root, confirmed)
    };
    let prompt = fixture.claimed.begin_prompt("Work in the named root.");
    assert!(
        prompt.prompt.ends_with("Work in the named root."),
        "the user's work reaches the provider"
    );
    assert_eq!(
        fixture.claimed.record(|record| record.session.status),
        SessionStatus::Active
    );
    assert_eq!(
        fixture
            .claimed
            .state
            .engram_root_capture(&fixture.claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    assert!(
        prompt.prompt.contains("source root is unconfirmed"),
        "{}",
        prompt.prompt
    );
    assert!(
        prompt.prompt.contains(&format!(
            "Reason: {}.",
            EngramUnconfirmedRootReason::LocalEntryWithoutEvent.sentence()
        )),
        "{}",
        prompt.prompt
    );
    let worktree = fixture.worktree.to_string_lossy().into_owned();
    let reason_line = prompt
        .prompt
        .lines()
        .find(|line| line.contains("Reason:"))
        .unwrap();
    assert!(
        !reason_line.contains(&worktree),
        "the reason names no path: {reason_line}"
    );
    for phrase in [
        "Your work is not blocked: Git, build, test and tool actions run normally.",
        "Only this turn's source, check and evaluation evidence is withheld from credit.",
        "To earn credit in a later turn, name the worktree or refresh its authority; neither takes effect in this turn.",
    ] {
        assert!(
            reason_line.contains(phrase),
            "missing {phrase:?}: {reason_line}"
        );
    }
    {
        let mut inner = fixture.claimed.state.inner.lock().unwrap();
        let index = inner
            .find_session_index(&fixture.claimed.session_id)
            .unwrap();
        inner.sessions[index].engram.named_root = named_root;
        journal_of(&mut inner, &fixture).confirmed = confirmed;
        assert!(
            matches!(
                engram_root_capture_explained_locked(&inner, &fixture.claimed.session_id),
                Ok(EngramRootCapture::Recorded {
                    state: EngramSourceRootState::Named,
                    ..
                })
            ),
            "the repaired authority is available for a later opening"
        );
        assert_eq!(
            engram_turn_root_capture_locked(&inner, &fixture.claimed.session_id),
            EngramRootCapture::Unconfirmed,
            "repair cannot relabel this turn's opening"
        );
    }
    fixture.claimed.end();
}
