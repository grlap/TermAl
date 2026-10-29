use super::*;

const EAGER_ROOTS: &[&str] = &[
    "federation", "codex", "preferences", "testRunCardsEpoch", "engramSessionObligations",
];

fn observed_collect(fixture: &FenceFixture) -> (PersistDelta, Vec<&'static str>) {
    let observer = Arc::new(PersistObserver::default());
    let scope = observer.begin_at(std::time::Instant::now());
    let delta = fixture.collect();
    scope.finish(true, &PersistTickTimings::default());
    let mut fields = observer.snapshot_at(std::time::Instant::now()).recent[0]
        .counts
        .metadata_cloned_fields
        .iter()
        .copied()
        .filter(|field| !EAGER_ROOTS.contains(field))
        .collect::<Vec<_>>();
    fields.sort_unstable();
    (delta, fields)
}

fn full_units(fixture: &FenceFixture) -> PersistMetadataUnits {
    project_metadata_units(&PersistedState::metadata_from_inner(
        &fixture.inner.lock().unwrap(),
    ))
    .unwrap()
}

fn assert_units(fixture: &FenceFixture, expected: &PersistMetadataUnits) {
    assert_eq!(
        &read_metadata_units_unchecked(metadata_units_connection(fixture)).unwrap(),
        expected
    );
}

#[test]
fn every_metadata_collection_mutable_borrow_marks_its_field() {
    let mut checked = Vec::new();
    macro_rules! check {
        ($field:ident, $wire:literal) => {{
            let mut fixture = FenceFixture::new();
            fixture.write(&fixture.collect()).unwrap();
            assert!(observed_collect(&fixture).1.is_empty());
            {
                let mut inner = fixture.inner.lock().unwrap();
                // A mutable borrow without a value change is deliberately
                // conservative. Its payload cannot be reached without marking.
                let _borrow = &mut *inner.$field;
            }
            let (delta, fields) = observed_collect(&fixture);
            assert_eq!(fields, vec![$wire]);
            let expected = full_units(&fixture);
            fixture.write(&delta).unwrap();
            assert_units(&fixture, &expected);
            assert!(observed_collect(&fixture).1.is_empty());
            checked.push($wire);
        }};
    }
    check!(projects, "projects");
    check!(
        engram_retired_work_authority_grants,
        "engramRetiredWorkAuthorityGrants"
    );
    check!(engram_work_source_roots, "engramWorkSourceRoots");
    check!(engram_named_root_journal, "engramNamedRootJournal");
    check!(engram_work_naming_history, "engramWorkNamingHistory");
    check!(
        engram_named_root_release_audit,
        "engramNamedRootReleaseAudit"
    );
    check!(engram_source_sightings, "engramSourceSightings");
    check!(
        pending_coordination_scope_deletions,
        "pendingCoordinationScopeDeletions"
    );
    check!(
        pending_response_board_project_detachments,
        "pendingResponseBoardProjectDetachments"
    );
    check!(
        ignored_discovered_codex_thread_ids,
        "ignoredDiscoveredCodexThreadIds"
    );
    check!(orchestrator_instances, "orchestratorInstances");
    check!(delegation_waits, "delegationWaits");
    check!(test_run_cards, "testRunCards");
    check!(test_run_registrations, "testRunRegistrations");
    check!(test_run_observations, "testRunObservations");
    check!(test_run_compacted, "testRunCompacted");
    check!(test_run_waits, "testRunWaits");
    check!(workspace_layouts, "workspaceLayouts");

    let fixture = FenceFixture::new();
    let mut inventory = full_units(&fixture)
        .into_iter()
        .filter_map(|((field, key), (kind, _))| {
            (key.is_empty()
                && matches!(kind.as_str(), "array" | "object")
                && !EAGER_ROOTS.contains(&field.as_str())
                && field != "sessions")
                .then_some(field)
        })
        .collect::<Vec<_>>();
    checked.sort_unstable();
    inventory.sort_unstable();
    assert_eq!(
        checked, inventory,
        "the sweep covers every persisted collection descriptor"
    );
}

#[test]
fn metadata_wrappers_track_vec_map_set_families_and_leave_reads_clean() {
    let mut vector: PersistTracked<Vec<u64>> = vec![1].into();
    let before = vector.stamp();
    assert_eq!(vector.iter().copied().collect::<Vec<_>>(), vec![1]);
    assert!(vector.stamp() == before);
    vector.push(2);
    assert!(vector.stamp() != before);
    let before = vector.stamp();
    *vector.get_mut(0).unwrap() = 3;
    assert!(vector.stamp() != before);
    let before = vector.stamp();
    for value in &mut vector {
        *value += 1;
    }
    assert!(vector.stamp() != before);

    let mut map: PersistTracked<BTreeMap<String, String>> =
        BTreeMap::from([("key".into(), "v".into())]).into();
    let before = map.stamp();
    assert_eq!(map.get("key").unwrap(), "v");
    assert!(map.stamp() == before);
    map.get_mut("key").unwrap().push('2');
    assert!(map.stamp() != before);
    let before = map.stamp();
    map.entry("new".into()).or_default().push('x');
    assert!(map.stamp() != before);
    let before = map.stamp();
    let _ = std::mem::take(&mut *map);
    assert!(map.stamp() != before);

    let mut set: PersistTracked<BTreeSet<String>> = BTreeSet::from(["a".into()]).into();
    let before = set.stamp();
    assert!(set.contains("a"));
    assert!(set.stamp() == before);
    set.insert("b".into());
    assert!(set.stamp() != before);
    let before = set.stamp();
    set.retain(|value| value != "a");
    assert!(set.stamp() != before);
}

#[test]
fn whole_wrapper_swap_take_and_restore_cannot_reuse_a_clean_field_stamp() {
    let mut fixture = FenceFixture::new();
    {
        let mut inner = fixture.inner.lock().unwrap();
        inner
            .pending_coordination_scope_deletions
            .insert("scope".into());
        inner
            .ignored_discovered_codex_thread_ids
            .insert("thread".into());
    }
    fixture.write(&fixture.collect()).unwrap();
    {
        let mut inner = fixture.inner.lock().unwrap();
        let StateInner {
            pending_coordination_scope_deletions,
            ignored_discovered_codex_thread_ids,
            ..
        } = &mut *inner;
        std::mem::swap(
            pending_coordination_scope_deletions,
            ignored_discovered_codex_thread_ids,
        );
    }
    let (delta, fields) = observed_collect(&fixture);
    assert_eq!(
        fields,
        vec![
            "ignoredDiscoveredCodexThreadIds",
            "pendingCoordinationScopeDeletions"
        ]
    );
    fixture.write(&delta).unwrap();
    assert_units(&fixture, &full_units(&fixture));

    let saved = std::mem::take(
        &mut fixture
            .inner
            .lock()
            .unwrap()
            .ignored_discovered_codex_thread_ids,
    );
    let (delta, fields) = observed_collect(&fixture);
    assert_eq!(fields, vec!["ignoredDiscoveredCodexThreadIds"]);
    fixture.write(&delta).unwrap();
    assert_units(&fixture, &full_units(&fixture));
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids = saved;
    let (delta, fields) = observed_collect(&fixture);
    assert_eq!(fields, vec!["ignoredDiscoveredCodexThreadIds"]);
    fixture.write(&delta).unwrap();
    assert_units(&fixture, &full_units(&fixture));
}

#[test]
fn failed_metadata_write_does_not_adopt_collection_generations() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .insert("retry".into());
    metadata_units_connection(&fixture).execute_batch(
        "CREATE TEMP TRIGGER refuse_collection BEFORE INSERT ON state_metadata_units
         WHEN new.field_name='ignoredDiscoveredCodexThreadIds' BEGIN SELECT RAISE(ABORT,'refuse'); END;"
    ).unwrap();
    assert!(fixture.write(&fixture.collect()).is_err());
    let (delta, fields) = observed_collect(&fixture);
    assert_eq!(fields, vec!["ignoredDiscoveredCodexThreadIds"]);
    // The failed writer discards its connection and its TEMP trigger.
    assert!(fixture.cache.connection.is_none());
    fixture.write(&delta).unwrap();
    assert!(observed_collect(&fixture).1.is_empty());
    assert_units(&fixture, &full_units(&fixture));
}

#[test]
fn mutation_after_capture_is_deferred_without_losing_its_generation() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .insert("first".into());
    let first_expected = full_units(&fixture);
    let first = fixture.collect();
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .insert("second".into());
    fixture.write(&first).unwrap();
    assert_units(&fixture, &first_expected);
    let (second, fields) = observed_collect(&fixture);
    assert_eq!(fields, vec!["ignoredDiscoveredCodexThreadIds"]);
    fixture.write(&second).unwrap();
    assert_units(&fixture, &full_units(&fixture));
}

#[test]
fn sparse_capture_reopens_cache_and_reloads_identical_units() {
    let mut fixture = FenceFixture::new();
    fixture.wait_record("retained-wait");
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .insert("retained".into());
    fixture.write(&fixture.collect()).unwrap();
    let expected = full_units(&fixture);
    fixture.cache = SqlitePersistConnectionCache::new();
    let (delta, fields) = observed_collect(&fixture);
    assert!(fields.is_empty());
    fixture.write(&delta).unwrap();
    assert_units(&fixture, &expected);
    let loaded = load_state(&fixture.root.path().join("termal.sqlite"))
        .unwrap()
        .unwrap();
    let mut actual = project_metadata_units(&PersistedState::metadata_from_inner(&loaded)).unwrap();
    let mut expected_after_boot = expected;
    // Startup binds the local host identity to this store. Every collection
    // and all other roots must still reload byte-for-byte.
    actual.retain(|(field, _), _| field != "federation");
    expected_after_boot.retain(|(field, _), _| field != "federation");
    assert_eq!(actual, expected_after_boot);
    let mut expected_federation = fixture.inner.lock().unwrap().federation.clone();
    expected_federation.local =
        Some(read_federation_host_identity(metadata_units_connection(&fixture)).unwrap());
    expected_federation.store_binding = normalize_local_user_facing_path(
        &fs::canonicalize(fixture.root.path().join("termal.sqlite"))
            .unwrap()
            .to_string_lossy(),
    );
    assert_eq!(loaded.federation, expected_federation);
}

#[test]
fn edited_sparse_images_preserve_insert_clear_and_whole_replacement() {
    for operation in ["insert", "clear", "replace"] {
        let mut fixture = FenceFixture::new();
        fixture
            .inner
            .lock()
            .unwrap()
            .ignored_discovered_codex_thread_ids
            .insert("original".into());
        fixture.write(&fixture.collect()).unwrap();
        let mut delta = fixture.collect();
        assert!(
            delta
                .metadata
                .metadata_collection_is_omitted("ignoredDiscoveredCodexThreadIds")
        );
        match operation {
            "insert" => {
                delta
                    .metadata
                    .ignored_discovered_codex_thread_ids
                    .insert("edited".into());
            }
            "clear" => delta.metadata.ignored_discovered_codex_thread_ids.clear(),
            "replace" => delta.metadata.ignored_discovered_codex_thread_ids = Default::default(),
            _ => unreachable!(),
        }
        delta.metadata = delta.metadata.clone().metadata_only();
        assert!(
            !delta
                .metadata
                .metadata_collection_is_omitted("ignoredDiscoveredCodexThreadIds")
        );
        fixture.write(&delta).unwrap();
        let stored = metadata_units_metadata(metadata_units_connection(&fixture));
        let expected = if operation == "insert" {
            BTreeSet::from(["edited".to_string()])
        } else {
            BTreeSet::new()
        };
        assert_eq!(*stored.ignored_discovered_codex_thread_ids, expected);
        let (next, fields) = observed_collect(&fixture);
        assert_eq!(fields, vec!["ignoredDiscoveredCodexThreadIds"]);
        fixture.write(&next).unwrap();
        assert_units(&fixture, &full_units(&fixture));
    }
}

#[test]
fn editing_a_selected_image_does_not_adopt_its_source_generation() {
    let mut fixture = FenceFixture::new();
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .insert("source".into());
    let mut delta = fixture.collect();
    delta.metadata.ignored_discovered_codex_thread_ids.clear();
    fixture.write(&delta).unwrap();
    let (next, fields) = observed_collect(&fixture);
    assert!(fields.contains(&"ignoredDiscoveredCodexThreadIds"));
    fixture.write(&next).unwrap();
    assert_units(&fixture, &full_units(&fixture));
    assert!(observed_collect(&fixture).1.is_empty());
}

#[test]
fn omitted_root_selection_cannot_prove_a_release_by_absence() {
    let mut fixture = FenceFixture::new();
    let selection = EngramWorkSourceRoot {
        store: EngramAuthorityStoreKey {
            database_path: fixture.root.path().join("authority.db"),
            project_id: "example/project".into(),
        },
        work_id: "work".into(),
        short_ref: "work".into(),
        claim_id: "claim".into(),
        claim_fence: 1,
        root: fixture.root.path().to_string_lossy().into_owned(),
        common_dir_key: "repo".into(),
        named_by_session: "session".into(),
        named_at: stamp_now(),
        generation: 1,
    };
    fixture
        .inner
        .lock()
        .unwrap()
        .engram_work_source_roots
        .push(selection.clone());
    fixture.write(&fixture.collect()).unwrap();
    let audit = EngramNamedRootReleaseAudit {
        id: "release".into(),
        acknowledged: false,
        selection,
        route: "operator".into(),
        reason: "test".into(),
        requested_by: None,
        lifecycle: "open".into(),
        released_at: stamp_now(),
        authority_owner: None,
        phase: None,
    };
    fixture
        .inner
        .lock()
        .unwrap()
        .engram_named_root_release_audit
        .push(audit.clone());
    let waiter = fixture.enqueue(PersistFenceTarget::EngramNamedRootRelease(Box::new(audit)));
    fixture.receive(false);
    let delta = fixture.collect();
    assert!(
        delta
            .metadata
            .metadata_collection_is_omitted("engramWorkSourceRoots")
    );
    assert!(!fixture.batch.pending[0].target.is_in_delta(&delta));
    fixture.write(&delta).unwrap();
    assert_eq!(poll(&waiter), None);
    fixture
        .inner
        .lock()
        .unwrap()
        .engram_work_source_roots
        .clear();
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&waiter), Some(Ok(())));
}

#[test]
fn synchronous_full_write_publishes_stamps_before_a_wrapper_is_restored() {
    let mut fixture = FenceFixture::new();
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .insert("original".into());
    fixture.write(&fixture.collect()).unwrap();
    {
        let mut inner = fixture.inner.lock().unwrap();
        let original = std::mem::replace(
            &mut inner.ignored_discovered_codex_thread_ids,
            BTreeSet::from(["synchronous".to_string()]).into(),
        );
        persist_state(&fixture.root.path().join("termal.sqlite"), &inner).unwrap();
        inner.ignored_discovered_codex_thread_ids = original;
    }
    let (delta, fields) = observed_collect(&fixture);
    assert_eq!(fields, vec!["ignoredDiscoveredCodexThreadIds"]);
    fixture.write(&delta).unwrap();
    assert_units(&fixture, &full_units(&fixture));
    assert!(observed_collect(&fixture).1.is_empty());
}

#[test]
fn wrapper_round_trip_without_a_commit_keeps_the_clean_stamp() {
    let mut fixture = FenceFixture::new();
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .insert("original".into());
    fixture.write(&fixture.collect()).unwrap();
    {
        let mut inner = fixture.inner.lock().unwrap();
        let original = std::mem::take(&mut inner.ignored_discovered_codex_thread_ids);
        inner.ignored_discovered_codex_thread_ids = original;
    }
    let (delta, fields) = observed_collect(&fixture);
    assert!(fields.is_empty());
    fixture.write(&delta).unwrap();
    assert_units(&fixture, &full_units(&fixture));
}
