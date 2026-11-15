// Shared CLI reads for acceptance requests, submissions, settings and recovery.
// Owns decoding and actual process observations, not recovery publication policy.
// Parent/settings rendering preserves typed causes; recovery filters at its own boundary.

fn acceptance_evaluation_transport_error(operation: &str, error: EngramTransportError) -> ApiError {
    ApiError::bad_gateway(format!(
        "{operation}: {}",
        acceptance_cli_read_error_detail(&error)
    ))
}

fn acceptance_cli_read_error_detail(error: &EngramTransportError) -> String {
    match error.code.as_deref() {
        Some(code) if error.kind == EngramTransportErrorKind::Remote => format!("{code}: {error}"),
        _ => error.to_string(),
    }
}

fn run_acceptance_evaluation_read(
    connection: &EngramConnectionConfig,
    args: &[String],
    timeout: Duration,
    host_workdir: &FsPath,
) -> std::result::Result<Value, EngramTransportError> {
    run_acceptance_evaluation_read_with_cli(
        connection,
        args,
        timeout,
        host_workdir,
        run_engram_cli_command_with_lock_retry,
    )
}

// Keep deterministic process observations at the same boundary as native reads.
fn run_acceptance_evaluation_read_with_cli(
    connection: &EngramConnectionConfig,
    args: &[String],
    timeout: Duration,
    host_workdir: &FsPath,
    run: impl FnOnce(
        &EngramConnectionConfig,
        &[&str],
        Duration,
        &str,
        &FsPath,
    ) -> std::result::Result<EngramCliOutput, EngramTransportError>,
) -> std::result::Result<Value, EngramTransportError> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run(
        connection,
        &args,
        timeout,
        ACCEPTANCE_EVALUATION_READER_LABEL,
        host_workdir,
    )?;
    let observation = EngramCliFailureObservation {
        exit: output.exit,
        stderr_bytes: output.stderr.len(),
    };
    let decoded = if is_acceptance_selected_note_read(&args) {
        // Exact selected-record refusals carry a structured code on stderr.
        // Retain the exit observation before the generic JSON reader erases it.
        decode_acceptance_selected_cli_output(output)
    } else {
        decode_acceptance_read_cli_output(output)
    };
    decoded.map_err(|mut error| {
        error.cli_failure = Some(observation);
        error
    })
}

fn decode_acceptance_read_cli_output(
    output: EngramCliOutput,
) -> std::result::Result<Value, EngramTransportError> {
    if output.success {
        return engram_decode_slice(&output.stdout, ACCEPTANCE_EVALUATION_READER_LABEL);
    }
    // Only the documented refusal exit plus a whole error envelope is an
    // Engram refusal. Crash output and prose never become authoritative codes.
    if output.exit.code() == Some(ACCEPTANCE_EVALUATION_REFUSAL_EXIT_CODE)
        && let Some((code, message, _)) = acceptance_evaluation_error_envelope(&output.stderr)
    {
        return Err(EngramTransportError::remote(EngramControlErrorBody {
            code,
            message,
        }));
    }
    decode_engram_cli_json(output, ACCEPTANCE_EVALUATION_READER_LABEL).map_err(|mut error| {
        error.code = Some("cli_read_failed".to_owned());
        error
    })
}
