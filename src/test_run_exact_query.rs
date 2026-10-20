// Owns exact GET selection against a captured registration content fence.
// Does not publish the index, scan history, or decide persistence durability.
// Split from test_runs_api.rs so native filesystem checks stay off the state lock.
struct TestRunExactQuery {
    run: TestRunSummary,
    directory: Option<PathBuf>,
    request_identity: Option<String>,
}

#[cfg(test)]
thread_local! {
    // Controlled supersession while the native identity check is off-lock.
    static TEST_TEST_RUN_QUERY_CANONICALIZED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

// Called on the blocking pool. Display keys are unsuitable here: Unix names
// can contain literal backslashes and distinct bytes with the same lossy text.
fn test_run_registered_disk_matches(row: &TestRunRegistration, disk: &TestRunDisk) -> bool {
    row.run_id == disk.run_id
        && disk.request_identity.as_ref() == Some(&row.request_identity)
        && disk.owner.as_deref() == Some(row.initiating_session_id.as_str())
        && disk.owner.as_deref() == Some(row.owning_session_id.as_str())
        && disk.started_at == row.started_at
        && fs::canonicalize(&row.run_directory)
            .ok()
            .zip(fs::canonicalize(&disk.run_dir).ok())
            .is_some_and(|(registered, indexed)| registered == indexed)
}

fn test_run_exact_query(
    state: &AppState,
    run_id: &str,
    diagnostics: &mut TestRunRegistrationDiagnostics,
) -> Result<Option<TestRunExactQuery>, ApiError> {
    // Capture the durable content fence before selecting an index candidate.
    let (registration, durable, entry) = {
        let inner = diagnostics.lock(&state.inner);
        (
            inner.test_run_registrations.get(run_id).cloned(),
            inner.test_runs.registered.contains(run_id),
            inner.test_runs.find(run_id).cloned(),
        )
    };
    let matches = entry.as_ref().is_some_and(|entry| {
        registration
            .as_ref()
            .is_none_or(|row| test_run_registered_disk_matches(row, &entry.disk))
    });
    #[cfg(test)]
    if let Some(changed) = TEST_TEST_RUN_QUERY_CANONICALIZED.with(|slot| slot.borrow_mut().take()) {
        changed();
    }
    let inner = diagnostics.lock(&state.inner);
    if inner.test_run_registrations.get(run_id) != registration.as_ref()
        || inner.test_runs.registered.contains(run_id) != durable
    {
        return Err(ApiError::conflict("test run registration changed during query; retry"));
    }
    let indexed = inner.test_runs.find(run_id);
    let same_index = match (&entry, indexed) {
        (Some(before), Some(now)) => Arc::ptr_eq(&before.disk, &now.disk),
        (None, None) => true,
        _ => false,
    };
    if matches && same_index {
        let entry = indexed.expect("matching captured index entry");
        let mut run = entry.summary.clone();
        run.registration = test_run_registration_view(&inner, &entry.disk);
        test_run_enrich_artifact_summary(&inner, &mut run, Some(&entry.disk));
        return Ok(Some(TestRunExactQuery {
            run,
            directory: Some(entry.disk.run_dir.clone()),
            request_identity: entry.disk.request_identity.clone(),
        }));
    }
    if let Some(row) = &registration {
        // A different indexed copy never shadows the acknowledged locator.
        let mut run = test_run_unavailable_artifact(test_run_registered_fallback(&inner, row));
        test_run_enrich_artifact_summary(&inner, &mut run, None);
        return Ok(Some(TestRunExactQuery {
            run,
            directory: None,
            request_identity: Some(row.request_identity.clone()),
        }));
    }
    if !same_index {
        return Err(ApiError::conflict("test run index changed during query; retry"));
    }
    Ok(None)
}
