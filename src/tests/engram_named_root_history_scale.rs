// Full-size durable history coverage without repeated naming RPCs.
use super::*;

#[test]
fn published_history_at_production_capacity_roundtrips_without_journal_pins() {
    let (claimed, _) = settled_root("published-history-full-size", "completed");
    claimed
        .state
        .run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    let mut inner = claimed.state.inner.lock().unwrap();
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    let notices = refresh_engram_source_root_notices_locked(&mut inner, index);
    acknowledge_engram_root_retirements_locked(&mut inner, &claimed.session_id, &notices);
    acknowledge_engram_source_root_notices_locked(&mut inner.sessions[index], &notices);
    let template = inner.engram_work_naming_history[0].retirements[0].clone();
    assert!(template.published && template.delivered);
    // Scale the durable payload once from a real retired notice, rather than
    // performing a thousand synchronous naming/publication RPC sequences.
    let history: Vec<_> = (0..ENGRAM_NAMED_ROOT_JOURNAL_LIMIT)
        .map(|number| {
            let mut notice = template.clone();
            notice.id = format!("published-history-notice-{number}");
            notice
        })
        .collect();
    inner.engram_work_naming_history[0].retirements = history.clone();
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty());
    assert_eq!(engram_named_root_capacity(&inner), 0);
    let bytes = serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap();
    drop(inner);
    let mut restored = serde_json::from_slice::<PersistedState>(&bytes)
        .unwrap()
        .into_inner()
        .unwrap();
    assert_eq!(restored.engram_work_naming_history[0].retirements, history);
    assert_eq!(engram_root_journal_census(&restored).limit, 1024);
    engram_compact_root_journal(&mut restored);
    assert!(restored.engram_named_root_journal.is_empty());
    assert_eq!(engram_named_root_capacity(&restored), 0);
    let index = restored.find_session_index(&claimed.session_id).unwrap();
    assert!(refresh_engram_source_root_notices_locked(&mut restored, index).is_empty());
}
