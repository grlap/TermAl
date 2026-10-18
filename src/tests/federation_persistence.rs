// Owns durable identity, reload, restore-quarantine and writer controls.
// Does not own transport or membership scenarios. Split from federation_registry.rs
// as its child module, using that parent fixture without duplicating it.
use super::*;

#[test]
fn federation_reload_preserves_identity_and_reservations_but_never_a_live_link() {
    let state = central();
    let snapshot = active(&state, peer(48001));
    reserve(&state, &snapshot, "retained", "s1").unwrap();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(loaded.federation.local.as_ref(), Some(&snapshot.local));
    assert!(loaded.federation.links.is_empty());
    assert_eq!(loaded.federation.placements.len(), 1);
    let record = &loaded.federation.peers[&snapshot.peer.host_id];
    assert_eq!(record.transport_generation, snapshot.transport_generation);
    assert_eq!(
        loaded.federation.placements["retained"]
            .peer
            .transport_generation,
        snapshot.transport_generation
    );
    assert!(!record.verified_on(loaded.federation.links.get(&snapshot.peer.host_id)));
}

#[test]
fn federation_moved_store_epoch_reset_does_not_clear_admission_quarantine() {
    let state = central();
    let snapshot = active(&state, peer(48001));
    let moved_root = TestTempRoot::create("federation-clone");
    let moved = moved_root.path().join("termal.sqlite");
    // The synchronous fixture has no cached writer/WAL connection.
    fs::copy(state.persistence_path.as_path(), &moved).unwrap();
    let loaded = load_state(&moved).unwrap().unwrap();
    assert!(loaded.federation_continuity_lost);
    assert!(federation_snapshot_locked(&loaded, &snapshot.peer.host_id).is_err());
    let mut target = domain_state();
    target.persistence_path = Arc::new(moved.clone());
    *target.inner.lock().unwrap() = loaded;
    let local = target
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    target.reset_federation_continuity(&local).unwrap();
    let next = target
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    assert_eq!(next.host_id, local.host_id);
    assert!(next.host_generation > local.host_generation);
    assert!(!target.inner.lock().unwrap().federation_continuity_lost);
    assert!(target.ensure_federation_admission_allowed().is_err());
    assert!(target.ensure_federation_peer_current(&snapshot).is_err());
    let reopened = load_state(&moved).unwrap().unwrap();
    assert_eq!(reopened.federation.local, Some(next));
    assert!(!reopened.federation_continuity_lost);
    assert!(reopened.federation.admission_quarantine.is_some());
    assert!(target.reset_federation_continuity(&local).is_err());
}

#[test]
fn federation_declared_same_path_restore_retains_capacity_after_fresh_verification() {
    let state = central();
    let record = peer(48001);
    let snapshot = active(&state, record.clone());
    reserve(&state, &snapshot, "original", "s1").unwrap();
    state.reset_federation_continuity(&snapshot.local).unwrap();
    assert_eq!(
        state.inner.lock().unwrap().federation.admission_quarantine,
        Some(FederationQuarantineReason::DeclaredRestore)
    );
    assert!(state.ensure_federation_admission_allowed().is_err());
    let enrolled = connected(&state, record);
    state.verify_federation_peer(&enrolled, checks()).unwrap();
    let verified = state
        .federation_peer_snapshot(&snapshot.peer.host_id)
        .unwrap();
    assert_eq!(verified.membership, FederationMembershipState::Verified);
    assert_eq!(
        state
            .transition_federation_peer(&verified, FederationMembershipState::Active)
            .unwrap_err()
            .message,
        FEDERATION_STALE_AUTHORITY
    );
    assert!(reserve(&state, &verified, "replacement", "s2").is_err());
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(loaded.federation.admission_quarantine.is_some());
    assert_eq!(loaded.federation.placements.len(), 1);
    assert_eq!(
        state.federation_registry_view().unwrap()["peers"][0]["allocatedSessions"],
        json!(1)
    );
}

#[test]
fn federation_detected_quarantine_is_durable_even_if_the_store_moves_back() {
    let state = central();
    active(&state, peer(48001));
    let original = fs::read(state.persistence_path.as_path()).unwrap();
    let moved_root = TestTempRoot::create("federation-moved-back");
    let moved = moved_root.path().join("termal.sqlite");
    fs::write(&moved, original).unwrap();
    let detected = load_state(&moved).unwrap().unwrap();
    assert_eq!(
        detected.federation.admission_quarantine,
        Some(FederationQuarantineReason::StoreBindingChanged)
    );
    // Read the new durable flag, then carry the resulting database back to
    // its original binding. A matching path cannot erase that observation.
    fs::copy(&moved, state.persistence_path.as_path()).unwrap();
    let restored = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(!restored.federation_continuity_lost);
    assert_eq!(
        restored.federation.admission_quarantine,
        Some(FederationQuarantineReason::StoreBindingChanged)
    );
}

#[test]
fn federation_failed_durability_denies_admission_until_exact_reconciliation() {
    let state = domain_state();
    let local = state
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_federation_write BEFORE INSERT ON app_state BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    assert!(
        state
            .configure_federation(FederationSettings {
                enabled: true,
                role: FederationRole::Central,
                central: Some(local)
            })
            .is_err()
    );
    assert!(!state.inner.lock().unwrap().federation_durable);
    assert!(state.federation_peer_snapshots().is_err());
    connection
        .execute_batch("DROP TRIGGER reject_federation_write;")
        .unwrap();
    drop(connection);
    state.reconcile_federation_durability().unwrap();
    assert!(state.inner.lock().unwrap().federation_durable);
}

#[test]
fn federation_stale_full_snapshot_cannot_erase_newer_enrollment() {
    let state = central();
    let old = PersistedState::from_inner(&state.inner.lock().unwrap());
    let snapshot = active(&state, peer(48001));
    assert!(persist_persisted_state_to_sqlite(state.persistence_path.as_path(), &old).is_err());
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(loaded.federation.peers.contains_key(&snapshot.peer.host_id));
}

#[test]
fn federation_partial_identity_is_corruption_not_permission_to_mint_another_host() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    seed_current_state_core_tables(&connection);
    seed_current_state_auxiliary_tables(&connection);
    connection
        .execute(
            "INSERT INTO meta(key,value) VALUES('federation_host_id','not-a-host')",
            [],
        )
        .unwrap();
    assert!(ensure_sqlite_state_schema(&connection).is_err());
    let count: u32 = connection
        .query_row(
            "SELECT count(*) FROM meta WHERE key='federation_host_generation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn federation_production_writer_confirms_the_exact_committed_domain_and_meta_identity() {
    let root = TestTempRoot::create("federation-real-writer");
    let path = root.path().join("termal.sqlite");
    let state = AppState::new_with_paths(
        root.path().to_string_lossy().into_owned(),
        path.clone(),
        root.path().join("orchestrators.json"),
    )
    .unwrap();
    let local = state
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    state
        .configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Central,
            central: Some(local),
        })
        .unwrap();
    let snapshot = active(&state, peer(48001));
    let placement = reserve(&state, &snapshot, "actual-writer", "s1").unwrap();
    let image = federation_persist_image(&PersistedState::from_inner(&state.inner.lock().unwrap()));
    let connection = rusqlite::Connection::open(&path).unwrap();
    let target = PersistFenceTarget::Federation(image.clone());
    assert!(target.is_already_durable(&connection).unwrap());
    let mut stale = image.clone();
    stale["domain"]["version"] = json!(0);
    assert!(
        !PersistFenceTarget::Federation(stale)
            .is_already_durable(&connection)
            .unwrap()
    );
    assert!(
        state
            .ensure_federation_placement_current(&placement)
            .is_ok()
    );
    drop(connection);
    state.shutdown_persist_blocking();
    let loaded = load_state(&path).unwrap().unwrap();
    assert_eq!(loaded.federation.placements["actual-writer"], placement);
    assert!(loaded.federation.links.is_empty());
}

#[test]
fn federation_read_only_identity_upgrade_fails_without_returning_a_fake_identity() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    seed_current_state_core_tables(&connection);
    seed_current_state_auxiliary_tables(&connection);
    connection.execute_batch("PRAGMA query_only=ON;").unwrap();
    assert!(ensure_federation_host_identity(&connection).is_err());
    assert!(read_federation_host_identity(&connection).is_err());
    let count: u32 = connection
        .query_row(
            "SELECT count(*) FROM meta WHERE key LIKE 'federation_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
