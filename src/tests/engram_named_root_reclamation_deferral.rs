// Pins how the automatic reclamation pass spends its shared budget: a root it
// reaches without its per-root share of the budget is deferred untouched, a
// pass defers at most one root, and the next pass starts with that root.
// Reuses the settled-root fixture; does not own cooldown length, read limits,
// pass spacing or canonical read semantics.

/// The per-root share of one pass's budget, from the pass's own constants.
fn per_root_share() -> Duration {
    ENGRAM_ROOT_RECLAMATION_BUDGET / ENGRAM_ROOT_RECLAMATION_READ_LIMIT as u32
}

/// Another retained root beside `entry`, named after it, with its own work,
/// claim and canonical run association.
fn add_sibling_root(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot, label: &str, named_at: &str)
    -> EngramWorkSourceRoot
{
    let mut inner = claimed.state.inner.lock().unwrap();
    let mut association = engram_root_journal(&inner.engram_named_root_journal, &entry.store, &entry.claim_id)
        .unwrap().clone();
    let mut root = entry.clone();
    root.work_id = format!("deferral-work-{label}");
    root.claim_id = format!("deferral-claim-{label}");
    root.named_at = named_at.to_owned();
    association.claim_id = root.claim_id.clone();
    let binding = association.read_binding.as_mut().unwrap();
    binding.work_id = root.work_id.clone();
    binding.claim_id = root.claim_id.clone();
    inner.engram_named_root_journal.push(association);
    inner.engram_work_source_roots.push(root.clone());
    root
}

fn unfocused(root: &EngramWorkSourceRoot) -> EngramRootReclamationTask {
    EngramRootReclamationTask { root: root.clone(), route: EngramRootReclamationRoute::Unfocused }
}

/// Whether a pass started anything for `root`: an authority transition for
/// its work, or a cooldown after its attempt.
fn touched(claimed: &ClaimedRoot, root: &EngramWorkSourceRoot) -> (bool, bool) {
    let inner = claimed.state.inner.lock().unwrap();
    let transition = inner.engram_work_naming_history.iter()
        .any(|history| history.store == root.store && history.work_id == root.work_id
            && history.transition.is_some());
    let cooldown = inner.engram_root_reclamation.cooldowns.iter().any(|(selection, _)| selection == root);
    (transition, cooldown)
}

/// Spend all but half of the per-root share during the pass's first task.
fn starve_after_first_task(claimed: &ClaimedRoot) {
    let clock = claimed.state.engram_budget_clock();
    let spent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
        if phase == "before_delta" && !spent.swap(true, std::sync::atomic::Ordering::SeqCst) {
            clock.advance(ENGRAM_ROOT_RECLAMATION_BUDGET - per_root_share() / 2);
        }
    }));
}

#[test]
fn a_root_reached_without_its_share_of_the_pass_budget_is_deferred_untouched() {
    let (claimed, first) = settled_root("reclaim-deferral", "completed");
    let starved = add_sibling_root(&claimed, &first, "starved", "2099-01-01T00:00:00.001Z");
    let later = add_sibling_root(&claimed, &first, "later", "2099-01-01T00:00:00.002Z");
    starve_after_first_task(&claimed);
    claimed.state.run_engram_root_reclamation_pass(vec![unfocused(&first), unfocused(&starved), unfocused(&later)]);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&first),
        "the first root had the whole budget and retired");
    assert_eq!(touched(&claimed, &starved), (false, false),
        "a root reached with less than its share left gets no transition and no cooldown");
    assert_eq!(touched(&claimed, &later), (false, false),
        "the pass ends at its one deferral: a later root is not attempted");
    // The next plan starts with the eligible deferred root, ahead of the
    // root named before it; the other one waits behind it.
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.iter().map(|task| &task.root).collect::<Vec<_>>(), vec![&starved, &later]);
    starve_after_first_task(&claimed);
    claimed.state.run_engram_root_reclamation_pass(plan);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(touched(&claimed, &starved).0, "the deferred root is attempted first, with a fresh budget");
    assert_eq!(touched(&claimed, &later), (false, false),
        "the root behind it is now the pass's one deferral");
}

/// `count` more retained roots, all named before any root this file defers.
fn add_earlier_roots(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot, count: usize)
    -> Vec<EngramWorkSourceRoot>
{
    (0..count).map(|number| add_sibling_root(claimed, entry, &format!("earlier-{number}"),
        &format!("2098-01-01T00:00:00.{number:03}Z"))).collect()
}

/// Defer `starved` in one pass whose first task is `first`.
fn defer_one(claimed: &ClaimedRoot, first: &EngramWorkSourceRoot, starved: &EngramWorkSourceRoot) {
    starve_after_first_task(claimed);
    claimed.state.run_engram_root_reclamation_pass(vec![unfocused(first), unfocused(starved)]);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert_eq!(touched(claimed, starved), (false, false), "deferred untouched");
}

#[test]
fn an_eligible_deferred_root_is_planned_first_even_beyond_the_read_limit() {
    let (claimed, first) = settled_root("reclaim-deferral-cutoff", "completed");
    let starved = add_sibling_root(&claimed, &first, "starved", "2099-01-01T00:00:00.001Z");
    defer_one(&claimed, &first, &starved);
    let earlier = add_earlier_roots(&claimed, &first, ENGRAM_ROOT_RECLAMATION_READ_LIMIT + 1);
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), ENGRAM_ROOT_RECLAMATION_READ_LIMIT);
    assert_eq!(plan[0].root, starved, "priority is given before the read-limit cutoff");
    assert_eq!(plan[1..].iter().map(|task| &task.root).collect::<Vec<_>>(),
        earlier[..ENGRAM_ROOT_RECLAMATION_READ_LIMIT - 1].iter().collect::<Vec<_>>(),
        "the other roots keep their ordinary order");
}

#[test]
fn a_pass_attempt_record_is_bounded_cleaned_and_pruned() {
    let (claimed, first) = settled_root("reclaim-attempt-bounds", "completed");
    let state = &claimed.state;
    let now = state.engram_budget_clock().now();
    let mut inner = state.inner.lock().unwrap();
    // Cleaned and cut to a fixed length.
    let noisy = format!("line one\n\tline\u{7}two {}", "x".repeat(400));
    engram_record_root_pass_attempt_locked(&mut inner, &first, now, &Err(ApiError::conflict(noisy)));
    let (status, detail) = inner.engram_root_reclamation.attempts[0].refusal.clone().unwrap();
    assert_eq!(status, 409);
    assert!(detail.starts_with("line one line two x"), "{detail}");
    assert!(!detail.chars().any(char::is_control), "{detail:?}");
    assert_eq!(detail.chars().count(), ENGRAM_ROOT_PASS_ATTEMPT_DETAIL_CHARS);
    assert!(detail.ends_with('…'));
    // One entry per selection; a completed attempt that kept the root says so.
    engram_record_root_pass_attempt_locked(&mut inner, &first, now, &Ok(false));
    assert_eq!(inner.engram_root_reclamation.attempts.len(), 1);
    assert_eq!(engram_root_pass_attempt_text(&inner, &first, now).unwrap(),
        "last automatic pass attempt 0 s ago: completed, and canonical authority kept the root");
    // A retirement keeps none.
    engram_record_root_pass_attempt_locked(&mut inner, &first, now, &Ok(true));
    assert!(inner.engram_root_reclamation.attempts.is_empty());
    drop(inner);
    // Never more entries than the named-root capacity, oldest dropped first.
    let siblings = (0..=ENGRAM_WORK_SOURCE_ROOT_LIMIT).map(|number| add_sibling_root(&claimed, &first,
        &format!("bounded-{number}"), &format!("2097-01-01T00:00:00.{number:03}Z"))).collect::<Vec<_>>();
    let mut inner = state.inner.lock().unwrap();
    for sibling in &siblings {
        engram_record_root_pass_attempt_locked(&mut inner, sibling, now, &Ok(false));
    }
    assert_eq!(inner.engram_root_reclamation.attempts.len(), ENGRAM_WORK_SOURCE_ROOT_LIMIT);
    assert!(engram_root_pass_attempt_text(&inner, &siblings[0], now).is_none(), "the oldest was dropped");
    // A selection that leaves by any route is never shown, and is pruned at
    // the next record.
    let gone = siblings[1].clone();
    inner.engram_work_source_roots.retain(|root| *root != gone);
    assert!(engram_root_pass_attempt_text(&inner, &gone, now).is_none());
    engram_record_root_pass_attempt_locked(&mut inner, &siblings[2], now, &Ok(false));
    assert!(!inner.engram_root_reclamation.attempts.iter().any(|attempt| attempt.root == gone));
}

#[test]
fn a_kept_root_with_an_eligible_reader_names_its_cooldown_in_the_in_flight_refusal() {
    let (claimed, entry) = settled_root("reclaim-attempt-cooldown", "live");
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1, "an eligible reader plans the root");
    claimed.state.run_engram_root_reclamation_pass(plan);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_work_source_roots.contains(&entry), "a live claim keeps its root");
    let context = acceptance_evaluation_in_flight_context_locked(&inner, &entry.store, &entry.work_id, &entry.claim_id)
        .expect("a recorded attempt gives the refusal its context");
    assert!(context.status.starts_with(&format!("cooling down for {} s after its last pass",
        ENGRAM_ROOT_RECLAMATION_COOLDOWN.as_secs())), "{}", context.status);
    assert!(context.status.contains("last automatic pass attempt 0 s ago: "), "{}", context.status);
    let error = acceptance_evaluation_naming_refusal("lead", EngramWorkNamingChange::InFlight, Some(&context));
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.message.starts_with(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT), "{}", error.message);
    assert!(error.message.contains(&context.status), "{}", error.message);
    let hint = ENGRAM_ROOT_RECLAMATION_COOLDOWN + ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER;
    assert!(error.message.contains(&format!(
        "As a suggested delay, not a promise that the transition settles: request the evaluation again in {} seconds",
        hint.as_secs())), "{}", error.message);
}

#[test]
fn a_schedule_that_plans_nothing_does_not_carry_the_deferral_further() {
    let (claimed, first) = settled_root("reclaim-deferral-empty-plan", "completed");
    let starved = add_sibling_root(&claimed, &first, "starved", "2099-01-01T00:00:00.001Z");
    defer_one(&claimed, &first, &starved);
    let earlier = add_earlier_roots(&claimed, &first, ENGRAM_ROOT_RECLAMATION_READ_LIMIT);
    let now = claimed.state.engram_budget_clock().now();
    {
        // Nothing is eligible at the next schedule: its plan is empty.
        let mut inner = claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_root_reclamation.deferred.as_ref(), Some(&starved));
        inner.engram_root_reclamation.cooldowns = inner.engram_work_source_roots.iter()
            .map(|root| (root.clone(), now + ENGRAM_ROOT_RECLAMATION_COOLDOWN)).collect();
    }
    assert_eq!(claimed.state.schedule_engram_root_reclamation(true), "no eligible reclamation pass was scheduled");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        assert!(inner.engram_root_reclamation.deferred.is_none(), "the next plan consumed the deferral");
        inner.engram_root_reclamation.cooldowns.clear();
    }
    assert_eq!(reclamation_plan(&claimed).iter().map(|task| &task.root).collect::<Vec<_>>(),
        earlier.iter().collect::<Vec<_>>(), "a later plan gives the once-deferred root no priority");
}

#[test]
fn a_replaced_or_ineligible_deferred_root_gets_no_priority() {
    for case in ["next generation", "cooling down"] {
        let (claimed, first) = settled_root(&format!("reclaim-deferral-{}", case.replace(' ', "-")), "completed");
        let starved = add_sibling_root(&claimed, &first, "starved", "2099-01-01T00:00:00.001Z");
        defer_one(&claimed, &first, &starved);
        let earlier = add_earlier_roots(&claimed, &first, ENGRAM_ROOT_RECLAMATION_READ_LIMIT);
        let now = claimed.state.engram_budget_clock().now();
        {
            let mut inner = claimed.state.inner.lock().unwrap();
            match case {
                "next generation" => {
                    let entry = inner.engram_work_source_roots.iter_mut().find(|root| **root == starved).unwrap();
                    entry.generation += 1;
                }
                _ => inner.engram_root_reclamation.cooldowns.push((starved.clone(), now + ENGRAM_ROOT_RECLAMATION_COOLDOWN)),
            }
        }
        let plan = reclamation_plan(&claimed);
        assert_eq!(plan.iter().map(|task| &task.root).collect::<Vec<_>>(), earlier.iter().collect::<Vec<_>>(),
            "{case}: the ordinary plan, with no priority carried to another selection or past eligibility");
    }
}
