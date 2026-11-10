// Native acceptance input is authenticated by the original host reservation
// and terminal. Reuse the producer's validation instead of interpreting its
// source_id results as launcher commit results. This consumer keeps acceptance's
// clean-source policy; the producer may still record unchanged local changes.

fn acceptance_native_local_changes(
    registration: &TestRunRegistration,
) -> Option<AcceptanceCommittedInputRefusal> {
    let source = &registration.declared_native.as_ref()?.source.source_id;
    (!source.changes.is_empty()
        || source
            .submodules
            .iter()
            .any(|child| !child.changes.is_empty()))
    .then(|| AcceptanceCommittedInputRefusal::NativeLocalChanges {
        run_id: registration.run_id.clone(),
    })
}

fn acceptance_registered_input_start(
    registration: &TestRunRegistration,
) -> AcceptanceCommittedInputRunStart {
    use AcceptanceCommittedInputRunStart as Start;
    if registration.declared_native.is_none() {
        return acceptance_committed_input_run_start(
            &registration.run_id,
            &registration.run_directory,
        );
    }
    let TestRunJson::Parsed(results, _, _) =
        acceptance_committed_input_read_results(&registration.run_directory)
    else {
        return Start::Unknown;
    };
    if engram_verification_record_results_are_terminal(registration, &results) {
        return Start::Started;
    }
    // The native host writes a running stage before publishing registration.
    // No terminal authority exists yet; disk alone cannot turn it into PASS.
    if registration.declared_terminal.is_none()
        && results.get("runId").and_then(Value::as_str) == Some(&registration.run_id)
        && results.get("state").and_then(Value::as_str) == Some("running")
        && results.get("ended").is_none_or(Value::is_null)
    {
        Start::Pending
    } else {
        Start::Unknown
    }
}

fn acceptance_registered_input_outcome(
    registration: &TestRunRegistration,
    current: &EngramSourceId,
) -> Result<Option<bool>, AcceptanceCommittedInputRefusal> {
    use AcceptanceCommittedInputRefusal as Refusal;
    let Some(start) = &registration.declared_native else {
        return acceptance_committed_input_run_outcome(
            &registration.run_id,
            &registration.run_directory,
            &current.commit,
        );
    };
    let directory = FsPath::new(&registration.run_directory);
    let (TestRunJson::Parsed(request, _, _), TestRunJson::Parsed(results, _, _)) = (
        test_run_read_json(&directory.join("request.json")),
        acceptance_committed_input_read_results(&registration.run_directory),
    ) else {
        return Err(Refusal::ScanIncomplete);
    };
    if registration.declared_terminal.is_none()
        && results.get("runId").and_then(Value::as_str) == Some(&registration.run_id)
        && results.get("state").and_then(Value::as_str) == Some("running")
    {
        return Ok(None);
    }
    // Rebuilding checks the immutable request identity, before/after owner,
    // complete source equality, exact terminal digest and native eligibility.
    // An incoherent newer run never permits selection of an older PASS.
    let body = engram_verification_record_body(registration, &request, &results)
        .map_err(|_| Refusal::ScanIncomplete)?;
    if start.source.source_id.commit != current.commit {
        return Ok(None);
    }
    if let Some(refusal) = acceptance_native_local_changes(registration) {
        return Err(refusal);
    }
    Ok(Some(
        body.run.outcome == EngramVerificationRecordRunOutcome::Passed
            && start.source.source_id == *current,
    ))
}
