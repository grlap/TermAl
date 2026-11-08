// Post-completion own-clear uses published lifecycle proof without reviving
// a claim, and a superseded authority acknowledgement remains a refusal.
use super::*;

fn clear(claimed: &EOCheckedTurn, entry: &EngramWorkSourceRoot)
    -> Result<EngramSourceRootResponse, ApiError>
{
    claimed.state.name_engram_source_root(&claimed.session_id, EngramSourceRootRequest {
        work: entry.short_ref.clone(), path: None,
    })
}

fn image(claimed: &EOCheckedTurn, entry: &EngramWorkSourceRoot) -> EngramAuthorityImage {
    EngramAuthorityImage::capture(&claimed.state.inner.lock().unwrap(),
        &entry.store, &entry.work_id).unwrap()
}

fn producer_writes(claimed: &EOCheckedTurn) -> usize {
    claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_bind").count()
}

fn completed_retirement(label: &str) -> (EOCheckedTurn, EngramWorkSourceRoot) {
    let (claimed, entry) = settled_root(label, "completed");
    clear(&claimed, &entry).expect("canonical completion retires the retained selection");
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry));
    (claimed, entry)
}

#[test]
fn already_completed_clear_is_repeatable_without_authority_or_producer_mutation() {
    let (claimed, entry) = completed_retirement("completion-clear-already-ended");
    let before = image(&claimed, &entry);
    let writes = producer_writes(&claimed);
    let reads = root_reads(&claimed);
    // A no-op must not prepare an image or require a working writer.
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let results = [clear(&claimed, &entry), clear(&claimed, &entry)];
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    for result in results {
        let result = result.expect("an acknowledged completed retirement is already ended");
        assert!(result.notice.contains("already ended by completion"));
        assert_eq!(result.work_id, entry.work_id);
        assert_eq!(result.generation, 0);
        assert!(result.root.is_none());
        assert!(result.sealed.is_none());
    }
    assert_eq!(image(&claimed, &entry), before, "no new owner, event, journal or generation");
    assert_eq!(producer_writes(&claimed), writes);
    assert_eq!(root_reads(&claimed), reads, "published exact proof needs no lifecycle reread");
}

#[test]
fn completion_with_a_retained_selection_still_requires_canonical_reconciliation() {
    let (claimed, entry) = settled_root("completion-clear-retained", "completed");
    let reads = root_reads(&claimed);
    let writes = producer_writes(&claimed);
    let result = clear(&claimed, &entry).expect("a retained completed binding can be retired");
    assert!(root_reads(&claimed) > reads);
    assert!(result.notice.contains("ended by completion"));
    assert!(result.root.is_none());
    assert_eq!(producer_writes(&claimed), writes, "an ended claim cannot emit a new event");
    let saved = image(&claimed, &entry);
    assert!(saved.selection.is_none());
    let notices = saved.history.retirements.iter()
        .filter(|notice| notice.selection == entry).collect::<Vec<_>>();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].published);
    assert_eq!(notices[0].read.run.state, "completed");
}

#[test]
fn a_genuinely_superseded_clear_is_refused_without_releasing_the_selection() {
    let (claimed, entry) = settled_root("completion-clear-stale-image", "completed");
    let other = claimed.state.clone();
    let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_changed = changed.clone();
    let work = entry.work_id.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "before_owner_check"
                && !hook_changed.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                other.inner.lock().unwrap().engram_work_naming_history.iter_mut()
                    .find(|history| history.work_id == work).unwrap()
                    .transition.as_mut().unwrap().id.push_str("-new-owner");
            }
        }));
    let writes = producer_writes(&claimed);
    let result = clear(&claimed, &entry);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(changed.load(std::sync::atomic::Ordering::SeqCst));
    let error = result.expect_err("stale authority is not completion success");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains(ENGRAM_AUTHORITY_IMAGE_SUPERSEDED_ERROR));
    assert!(error.message.contains("retained_name_release_unconfirmed"));
    assert!(error.message.contains("Refresh authority in the original store"));
    let saved = image(&claimed, &entry);
    assert_eq!(saved.selection, Some(entry));
    assert!(saved.history.retirements.is_empty());
    assert!(saved.history.transition.unwrap().id.ends_with("-new-owner"));
    assert_eq!(producer_writes(&claimed), writes);
}

#[test]
fn a_competing_completed_retirement_does_not_turn_a_stale_ack_into_success() {
    let (claimed, entry) = settled_root("completion-clear-competing-owner", "completed");
    let other = claimed.state.clone();
    let raced = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_raced = raced.clone();
    let selection = entry.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "before_owner_check"
                && !hook_raced.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                assert!(other.recover_one_engram_root_until(&selection,
                    &EngramRootReclamationRoute::Unfocused,
                    other.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET).unwrap());
            }
        }));
    let writes = producer_writes(&claimed);
    let result = clear(&claimed, &entry);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(raced.load(std::sync::atomic::Ordering::SeqCst));
    let error = result.expect_err("the current attempt's stale ACK stays refused");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains(ENGRAM_AUTHORITY_IMAGE_SUPERSEDED_ERROR));
    assert_eq!(producer_writes(&claimed), writes);
    let settled = image(&claimed, &entry);
    assert!(settled.selection.is_none());
    assert_eq!(settled.history.retirements.len(), 1);
    assert!(settled.history.retirements[0].published);
    // A subsequent call reads that exact completed result, without claiming
    // that the refused attempt itself performed the retirement.
    assert!(clear(&claimed, &entry).unwrap().notice.contains("already ended by completion"));
    assert_eq!(image(&claimed, &entry), settled);
}

#[test]
fn completed_proof_cannot_clear_a_replacement_or_a_foreign_namers_binding() {
    for case in ["replacement", "foreign", "pending", "store", "new_claim"] {
        let (claimed, entry) = completed_retirement(&format!("completion-clear-{case}"));
        let mut replacement = entry.clone();
        replacement.claim_id.push_str("-new-claim");
        replacement.generation += 1;
        {
            let mut inner = claimed.state.inner.lock().unwrap();
            match case {
                "replacement" => inner.engram_work_source_roots.push(replacement.clone()),
                "foreign" => inner.engram_work_naming_history[0].retirements[0]
                    .selection.named_by_session = "another-session".to_owned(),
                "pending" => inner.engram_work_naming_history[0].transition.as_mut().unwrap()
                    .phase = EngramAuthorityPhase::Candidate,
                "store" => inner.engram_work_naming_history[0].store.project_id.push_str("-other"),
                "new_claim" => {
                    let mut binding = inner.engram_work_naming_history[0].retirements[0].binding.clone();
                    binding.claim_id.push_str("-new-claim");
                    let index = inner.find_session_index(&claimed.session_id).unwrap();
                    inner.sessions[index].engram.work_binding = Some(binding);
                }
                _ => unreachable!(),
            }
            assert!(engram_completed_own_root_clear_locked(&inner, &entry.store,
                &claimed.session_id, &entry.short_ref).is_none(), "{case}");
        }
        if case == "replacement" {
            let writes = producer_writes(&claimed);
            let result = clear(&claimed, &entry);
            assert!(result.is_err(), "old completion cannot authorize a new claim's clear");
            assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&replacement));
            assert_eq!(producer_writes(&claimed), writes);
        }
    }
}

#[test]
fn a_completed_result_is_rechecked_after_the_held_claim_read() {
    let (claimed, entry) = completed_retirement("completion-clear-held-read-race");
    let mut replacement = entry.clone();
    replacement.claim_id.push_str("-new-claim");
    replacement.generation += 1;
    let other = claimed.state.clone();
    let inserted = replacement.clone();
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            other.inner.lock().unwrap().engram_work_source_roots.push(inserted);
        }));
    });
    let writes = producer_writes(&claimed);
    let error = clear(&claimed, &entry).expect_err("a replacement invalidates the completed no-op");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&replacement));
    assert_eq!(producer_writes(&claimed), writes);
}
