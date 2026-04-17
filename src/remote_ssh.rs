// remote backend bridge — ssh argv, input validation, health probing.
//
// termal's remote mode lets the local instance act as a proxy for another
// termal running on a different machine. every termal instance exposes the
// same http api on REMOTE_SERVER_PORT; remote mode just spawns an openssh
// child with `-L <local>:127.0.0.1:<REMOTE_SERVER_PORT>` and then makes the
// local ui talk to the forwarded port. there is no separate wire protocol:
// the remote termal does not know it is being proxied, it just serves http.
//
// security. `remote_id`, `ssh_host`, and `ssh_user` come from user config
// and flow straight into the argv we hand to `ssh`. a hostile value like
// `-oProxyCommand=...`, `-F/path`, or a stray `--` would let the "hostname"
// inject openssh options and run arbitrary commands on the local machine.
// the validators here (`validate_remote_id_value`,
// `validate_remote_ssh_host_value`, `validate_remote_ssh_user_value`)
// reject anything that is not a narrow alphanumeric/punctuation alphabet,
// and `remote_ssh_command_args` always emits a literal `"--"` separator
// before the `<user>@<host>` target so openssh cannot re-interpret it even
// if a validator is relaxed in the future. the tests in
// `src/tests/remote.rs` (`rejects_remote_settings_with_*`,
// `remote_ssh_command_args_insert_double_dash_before_target`) pin this.
//
// error sanitization. the remote returns arbitrary http bodies and we fold
// those into our own error messages. `sanitize_remote_error_body` strips
// control characters, collapses whitespace, and truncates to
// MAX_REMOTE_ERROR_BODY_CHARS; `decode_remote_json` caps the raw read at
// MAX_REMOTE_ERROR_BODY_BYTES so a huge or adversarial response cannot
// flood the local log file or the ui error toasts.
//
// health probe. once the ssh child is up we poll `/api/health` through the
// forward with `wait_for_remote_health` until the remote responds `ok:true`
// or REMOTE_STARTUP_TIMEOUT elapses; only then do we consider the tunnel
// usable. if the ssh child dies during the wait we capture its stderr tail
// and surface that instead of a generic timeout.
//
// this file was extracted from `src/remote.rs` and is pulled back in as a
// flat `include!()` fragment, so everything here shares the surrounding
// module's types and imports.

/// Appends the scope's session or project id to a query string list the
/// caller is building for a forwarded remote request.
fn apply_remote_scope_to_query(scope: &RemoteScope, query: &mut Vec<(String, String)>) {
    if let Some(remote_session_id) = scope.remote_session_id.as_deref() {
        query.push(("sessionId".to_owned(), remote_session_id.to_owned()));
    } else if let Some(remote_project_id) = scope.remote_project_id.as_deref() {
        query.push(("projectId".to_owned(), remote_project_id.to_owned()));
    }
}

/// Merges the scope's session or project id into a JSON request body
/// destined for a forwarded remote request, creating an empty object if
/// the caller passed a non-object value.
fn apply_remote_scope_to_body(scope: &RemoteScope, body: Value) -> Value {
    let mut object = match body {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if let Some(remote_session_id) = scope.remote_session_id.as_deref() {
        object.insert(
            "sessionId".to_owned(),
            Value::String(remote_session_id.to_owned()),
        );
    } else if let Some(remote_project_id) = scope.remote_project_id.as_deref() {
        object.insert(
            "projectId".to_owned(),
            Value::String(remote_project_id.to_owned()),
        );
    }
    Value::Object(object)
}

/// Reserves a local TCP port from the
/// `REMOTE_FORWARD_PORT_START..=REMOTE_FORWARD_PORT_END` pool for an
/// outgoing ssh `-L` forward, wrapping around when the pool is exhausted.
fn allocate_remote_forward_port() -> u16 {
    loop {
        let current = NEXT_REMOTE_FORWARD_PORT.fetch_add(1, Ordering::SeqCst);
        if current <= REMOTE_FORWARD_PORT_END {
            return current;
        }
        NEXT_REMOTE_FORWARD_PORT.store(REMOTE_FORWARD_PORT_START, Ordering::SeqCst);
    }
}

/// Rejects with a 400 any `RemoteConfig` that is disabled, has an unsafe
/// id, or (for SSH transport) has a host/user value that would be unsafe
/// to pass to `ssh`. Gate for every call site that is about to build an
/// ssh argv or issue a forwarded request.
fn validate_remote_connection_config(remote: &RemoteConfig) -> Result<(), ApiError> {
    if !remote.enabled {
        return Err(ApiError::bad_request(format!(
            "remote `{}` is disabled",
            remote.name
        )));
    }
    validate_remote_id_value(remote.id.trim())?;
    match remote.transport {
        RemoteTransport::Local => Ok(()),
        RemoteTransport::Ssh => {
            normalized_remote_ssh_host(remote)?;
            normalized_remote_ssh_user(remote)?;
            Ok(())
        }
    }
}

/// Builds the validated `<user>@<host>` (or bare `<host>`) string used as
/// the final positional argument in the `ssh` argv.
fn remote_ssh_target(remote: &RemoteConfig) -> Result<String, ApiError> {
    let host = normalized_remote_ssh_host(remote)?;
    let user = normalized_remote_ssh_user(remote)?;
    Ok(match user {
        Some(user) => format!("{user}@{host}"),
        None => host,
    })
}

/// Assembles the full validated argv for the `ssh` child: batch-mode
/// options, the `-L <forwarded_port>:127.0.0.1:<REMOTE_SERVER_PORT>`
/// forward, a literal `--` separator, the `<user>@<host>` target, and
/// (for `ManagedServer` mode) a remote `termal server` command. The `--`
/// is always inserted so host/user can never be reparsed as ssh options,
/// even if a future change relaxes the validators — pinned by
/// `remote_ssh_command_args_insert_double_dash_before_target` in tests.
fn remote_ssh_command_args(
    remote: &RemoteConfig,
    forwarded_port: u16,
    mode: RemoteProcessMode,
) -> Result<Vec<String>, ApiError> {
    let target = remote_ssh_target(remote)?;
    let mut args = vec![
        "-T".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "ExitOnForwardFailure=yes".to_owned(),
        "-o".to_owned(),
        "ServerAliveInterval=15".to_owned(),
        "-o".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        "-p".to_owned(),
        remote.port.unwrap_or(DEFAULT_SSH_REMOTE_PORT).to_string(),
        "-L".to_owned(),
        format!("{forwarded_port}:127.0.0.1:{REMOTE_SERVER_PORT}"),
    ];
    if matches!(mode, RemoteProcessMode::TunnelOnly) {
        args.push("-N".to_owned());
    }
    args.push("--".to_owned());
    args.push(target);
    if matches!(mode, RemoteProcessMode::ManagedServer) {
        args.push("termal".to_owned());
        args.push("server".to_owned());
    }
    Ok(args)
}

/// Returns the trimmed, validated ssh host for a remote, or a 400 if the
/// host is missing/empty or would be unsafe to pass through the ssh argv.
fn normalized_remote_ssh_host(remote: &RemoteConfig) -> Result<String, ApiError> {
    let host = remote
        .host
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(format!("remote `{}` is missing an SSH host", remote.name))
        })?;
    validate_remote_ssh_host_value(host, &remote.name)?;
    Ok(host.to_owned())
}

/// Returns the trimmed, validated ssh user for a remote (or `None` when
/// unset so ssh uses the default), or a 400 if the value would be unsafe
/// to embed into the `<user>@<host>` target.
fn normalized_remote_ssh_user(remote: &RemoteConfig) -> Result<Option<String>, ApiError> {
    let Some(user) = remote
        .user
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    validate_remote_ssh_user_value(user, &remote.name)?;
    Ok(Some(user.to_owned()))
}

/// Rejects a remote id that is empty or contains anything outside
/// `[A-Za-z0-9._\-_]`. Guards against ids being used as filesystem or
/// URL path components and against shell-metacharacter injection via the
/// id (pinned by `rejects_remote_settings_with_unsafe_remote_id`).
fn validate_remote_id_value(id: &str) -> Result<(), ApiError> {
    if id.is_empty() {
        return Err(ApiError::bad_request("remote id cannot be empty"));
    }
    if !id
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.'))
    {
        return Err(ApiError::bad_request(format!(
            "remote id `{id}` contains unsupported characters",
        )));
    }
    Ok(())
}

/// Rejects an ssh host that starts with `-`, contains no alphanumeric
/// byte, or uses any character outside the narrow
/// `[A-Za-z0-9.\-_:\[\]%]` alphabet needed for DNS names, ipv4/ipv6
/// literals, and scope-ids. Guards against hosts like
/// `-oProxyCommand=...`, `-F/path`, or `--` being reinterpreted as
/// openssh options and executing arbitrary local commands (pinned by
/// `rejects_remote_settings_with_invalid_ssh_host`).
fn validate_remote_ssh_host_value(host: &str, remote_name: &str) -> Result<(), ApiError> {
    if host.starts_with('-') {
        return Err(ApiError::bad_request(format!(
            "remote `{remote_name}` has an invalid SSH host",
        )));
    }
    if !host.bytes().any(|byte| byte.is_ascii_alphanumeric())
        || !host.bytes().all(|byte| {
            matches!(
                byte,
                b'a'..=b'z'
                    | b'A'..=b'Z'
                    | b'0'..=b'9'
                    | b'.'
                    | b'-'
                    | b'_'
                    | b':'
                    | b'['
                    | b']'
                    | b'%'
            )
        })
    {
        return Err(ApiError::bad_request(format!(
            "remote `{remote_name}` has an invalid SSH host",
        )));
    }
    Ok(())
}

/// Rejects an ssh user that contains `@` (which would mangle the final
/// `<user>@<host>` target and let the user field smuggle in a different
/// host) or any character outside `[A-Za-z0-9.\-_]`. Pinned by
/// `rejects_remote_settings_with_invalid_ssh_user`.
fn validate_remote_ssh_user_value(user: &str, remote_name: &str) -> Result<(), ApiError> {
    if user.contains('@')
        || !user.bytes().all(
            |byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'_'),
        )
    {
        return Err(ApiError::bad_request(format!(
            "remote `{remote_name}` has an invalid SSH user",
        )));
    }
    Ok(())
}

/// Polls the forwarded `/api/health` endpoint every
/// `REMOTE_HEALTH_POLL_INTERVAL` until the remote reports `ok:true`, the
/// ssh child exits, or `REMOTE_STARTUP_TIMEOUT` elapses. On success
/// returns the `(handle, HealthResponse)` pair so the caller keeps the
/// live child; on failure the child is reaped and a human-readable error
/// (with the ssh stderr tail when available) is returned.
fn wait_for_remote_health(
    client: &BlockingHttpClient,
    base_url: &str,
    mut handle: RemoteProcessHandle,
) -> std::result::Result<(RemoteProcessHandle, HealthResponse), String> {
    let started_at = Instant::now();
    loop {
        if let Ok(payload) = remote_healthcheck(client, base_url) {
            return Ok((handle, payload));
        }
        match handle.child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "{} exited with {}{}",
                    handle.mode.label(),
                    status,
                    read_process_stderr_suffix(&mut handle.child)
                ));
            }
            Ok(None) => {}
            Err(err) => {
                return Err(format!("failed to inspect SSH process: {err}"));
            }
        }
        if started_at.elapsed() >= REMOTE_STARTUP_TIMEOUT {
            let _ = handle.child.kill();
            let _ = handle.child.wait();
            return Err(format!("{} timed out", handle.mode.label()));
        }
        thread::sleep(REMOTE_HEALTH_POLL_INTERVAL);
    }
}

/// Returns the canonical user-facing error string for "ssh tunnel
/// refused to come up for remote X", intentionally free of exit codes,
/// stderr snippets, and port numbers so those transport details do not
/// bleed into the ui (pinned by
/// `remote_connection_issue_message_hides_transport_details`).
fn remote_connection_issue_message(remote_name: &str) -> String {
    format!(
        "Could not connect to remote \"{remote_name}\" over SSH. Check the host, network, and SSH settings, then try again."
    )
}

/// Returns the canonical user-facing error string for "the local ssh
/// binary could not be spawned", nudging the user toward installing
/// OpenSSH on PATH instead of surfacing raw errno/spawn details (pinned
/// by `local_ssh_start_issue_message_hides_transport_details`).
fn local_ssh_start_issue_message(remote_name: &str) -> String {
    format!(
        "Could not start the local SSH client for remote \"{remote_name}\". Verify OpenSSH is installed and available on PATH, then try again."
    )
}

/// Drains whatever ssh has written to stderr so far and formats it as
/// `": <trimmed>"` for appending to a higher-level error, or returns an
/// empty string if there is nothing useful. Consumes the child's stderr
/// handle, so only call this once the child has exited.
fn read_process_stderr_suffix(child: &mut Child) -> String {
    let mut detail = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut detail);
    }
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(": {trimmed}")
    }
}

/// Issues one short `GET /api/health` against the forwarded base URL and
/// returns the decoded `HealthResponse` only if the remote reports
/// `ok:true`; the short `REMOTE_HEALTH_TIMEOUT` keeps the caller's poll
/// loop responsive.
fn remote_healthcheck(
    client: &BlockingHttpClient,
    base_url: &str,
) -> Result<HealthResponse> {
    let response = client
        .get(format!("{base_url}/api/health"))
        .timeout(REMOTE_HEALTH_TIMEOUT)
        .send()
        .with_context(|| format!("failed to contact {base_url}/api/health"))?;
    let payload: HealthResponse =
        decode_remote_json(response).map_err(|err| anyhow!(err.message))?;
    if payload.ok {
        Ok(payload)
    } else {
        Err(anyhow!("remote health endpoint returned ok=false"))
    }
}

/// Scrubs a remote-supplied error body for safe inclusion in local
/// errors/logs: drops control characters, collapses whitespace, and
/// truncates to `MAX_REMOTE_ERROR_BODY_CHARS` on a char boundary (adding
/// `"..."`). Returns `None` if nothing printable remains. Guards against
/// a hostile or oversized remote body flooding the log file or ui.
fn sanitize_remote_error_body(raw: &str) -> Option<String> {
    let sanitized = raw
        .chars()
        .filter_map(|ch| match ch {
            '\r' | '\n' | '\t' => Some(' '),
            _ if ch.is_control() => None,
            _ => Some(ch),
        })
        .collect::<String>();
    let collapsed = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut chars = trimmed.chars();
    let mut limited = chars
        .by_ref()
        .take(MAX_REMOTE_ERROR_BODY_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        // Truncate by char boundary, not byte offset, to avoid panicking on
        // multi-byte UTF-8 sequences (emoji, CJK, etc.).
        let truncate_at = limited
            .char_indices()
            .nth(MAX_REMOTE_ERROR_BODY_CHARS - 3)
            .map(|(i, _)| i)
            .unwrap_or(limited.len());
        limited.truncate(truncate_at);
        limited.push_str("...");
    }
    Some(limited)
}

/// Decodes a blocking HTTP response from the remote into `T`, or folds a
/// non-2xx response into an `ApiError` whose message is derived from the
/// remote's own error body (sanitized). Caps the error body read at
/// `MAX_REMOTE_ERROR_BODY_BYTES` so a pathological remote cannot force
/// an unbounded allocation on the local side.
fn decode_remote_json<T: DeserializeOwned>(response: BlockingHttpResponse) -> Result<T, ApiError> {
    let status = response.status();
    if !status.is_success() {
        let mut raw = Vec::new();
        response
            .take((MAX_REMOTE_ERROR_BODY_BYTES + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|err| {
                ApiError::bad_gateway(format!("failed to read remote response body: {err}"))
            })?;
        if raw.len() > MAX_REMOTE_ERROR_BODY_BYTES {
            return Err(ApiError::from_status(
                status,
                "remote error response too large".to_owned(),
            ));
        }
        if let Ok(error) = serde_json::from_slice::<ErrorResponse>(&raw) {
            let message = sanitize_remote_error_body(&error.error).unwrap_or_else(|| {
                format!("remote request failed with status {}", status.as_u16())
            });
            return Err(ApiError::from_status(status, message));
        }
        let raw = String::from_utf8_lossy(&raw);
        let message = sanitize_remote_error_body(raw.as_ref())
            .unwrap_or_else(|| format!("remote request failed with status {}", status.as_u16()));
        return Err(ApiError::from_status(status, message));
    }

    let raw = response.text().map_err(|err| {
        ApiError::bad_gateway(format!("failed to read remote response body: {err}"))
    })?;
    serde_json::from_str(&raw)
        .map_err(|err| ApiError::bad_gateway(format!("failed to decode remote response: {err}")))
}

/// Percent-encodes `value` the way JavaScript's `encodeURIComponent`
/// does (keeping `A-Za-z0-9-_.~` literal and hex-encoding everything
/// else) so remote session/project ids can be spliced into paths and
/// query strings without breaking URL parsing on the remote side.
fn encode_uri_component(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::new();
    for byte in value.bytes() {
        if matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{:02X}", byte);
        }
    }
    encoded
}

impl RemoteProcessMode {
    /// Human-readable name for the ssh child mode, used when building
    /// startup-failure and timeout error messages.
    fn label(self) -> &'static str {
        match self {
            Self::ManagedServer => "managed SSH session",
            Self::TunnelOnly => "SSH tunnel",
        }
    }
}
