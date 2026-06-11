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

#[derive(Clone, Debug, PartialEq, Eq)]
enum RemoteActionScript {
    Posix(String),
    WindowsPowerShell(String),
}

impl RemoteActionScript {
    fn command_args(&self, remote: &RemoteConfig) -> Result<Vec<String>, ApiError> {
        match self {
            Self::Posix(script) => remote_ssh_one_shot_command_args(remote, script),
            Self::WindowsPowerShell(script) => {
                remote_ssh_powershell_one_shot_command_args(remote, script)
            }
        }
    }
}

/// Assembles a one-shot SSH command that runs a POSIX shell script on the
/// remote host without allocating a persistent tunnel.
fn remote_ssh_one_shot_command_args(
    remote: &RemoteConfig,
    script: &str,
) -> Result<Vec<String>, ApiError> {
    let target = remote_ssh_target(remote)?;
    let args = vec![
        "-T".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "ConnectTimeout=10".to_owned(),
        "-o".to_owned(),
        "ServerAliveInterval=15".to_owned(),
        "-o".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        "-p".to_owned(),
        remote.port.unwrap_or(DEFAULT_SSH_REMOTE_PORT).to_string(),
        "--".to_owned(),
        target,
        "sh".to_owned(),
        "-lc".to_owned(),
        shell_quote(script),
    ];
    Ok(args)
}

/// Assembles a one-shot SSH command that runs an encoded PowerShell script on a
/// Windows remote host without allocating a persistent tunnel.
fn remote_ssh_powershell_one_shot_command_args(
    remote: &RemoteConfig,
    script: &str,
) -> Result<Vec<String>, ApiError> {
    let target = remote_ssh_target(remote)?;
    let args = vec![
        "-T".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "ConnectTimeout=10".to_owned(),
        "-o".to_owned(),
        "ServerAliveInterval=15".to_owned(),
        "-o".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        "-p".to_owned(),
        remote.port.unwrap_or(DEFAULT_SSH_REMOTE_PORT).to_string(),
        "--".to_owned(),
        target,
        "powershell".to_owned(),
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-ExecutionPolicy".to_owned(),
        "Bypass".to_owned(),
        "-EncodedCommand".to_owned(),
        powershell_encoded_command(script),
    ];
    Ok(args)
}

/// Quotes one shell word using the POSIX single-quote pattern.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Quotes one PowerShell single-quoted string literal.
fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn powershell_encoded_command(script: &str) -> String {
    use base64::Engine as _;

    let mut bytes = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteInstallPlatform {
    Posix,
    Windows,
}

const POSIX_METADATA_PLATFORM_SED: &str =
    r#"s/.*"platform"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p"#;
const POSIX_METADATA_SOURCE_PATH_SED: &str =
    r#"s/.*"sourcePath"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p"#;

fn looks_like_windows_source_path(source_path: &str) -> bool {
    let mut chars = source_path.chars();
    let drive_prefix = matches!(
        (chars.next(), chars.next()),
        (Some('A'..='Z' | 'a'..='z'), Some(':'))
    );
    drive_prefix || source_path.contains('\\') || source_path.starts_with("~\\")
}

fn validate_remote_source_path(
    source_path: &str,
) -> Result<(String, RemoteInstallPlatform), ApiError> {
    let trimmed = source_path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(
            "remote TermAl checkout path cannot be empty",
        ));
    }
    if trimmed.chars().count() > 4096 {
        return Err(ApiError::bad_request(
            "remote TermAl checkout path is too long",
        ));
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return Err(ApiError::bad_request(
            "remote TermAl checkout path cannot contain control characters",
        ));
    }

    let platform = if looks_like_windows_source_path(trimmed) {
        RemoteInstallPlatform::Windows
    } else {
        RemoteInstallPlatform::Posix
    };
    // POSIX metadata extraction below is deliberately shell-only and uses a
    // small sed expression, so values that would require JSON unescaping are
    // rejected locally before registration writes `remote-install.json`.
    if platform == RemoteInstallPlatform::Posix && trimmed.contains('"') {
        return Err(ApiError::bad_request(
            "remote TermAl checkout path cannot contain quotes",
        ));
    }
    Ok((trimmed.to_owned(), platform))
}

fn remote_install_metadata_json(
    source_path: &str,
    platform: RemoteInstallPlatform,
) -> Result<String, ApiError> {
    let platform = match platform {
        RemoteInstallPlatform::Posix => "posix",
        RemoteInstallPlatform::Windows => "windows",
    };
    serde_json::to_string(&json!({
        "platform": platform,
        "sourcePath": source_path,
    }))
    .map_err(|err| ApiError::internal(format!("failed to build remote metadata: {err}")))
}

fn remote_register_script(source_path: &str) -> Result<RemoteActionScript, ApiError> {
    let (source_path, platform) = validate_remote_source_path(source_path)?;
    match platform {
        RemoteInstallPlatform::Posix => Ok(RemoteActionScript::Posix(remote_posix_register_script(
            &source_path,
        )?)),
        RemoteInstallPlatform::Windows => Ok(RemoteActionScript::WindowsPowerShell(
            remote_windows_register_script(&source_path),
        )),
    }
}

fn remote_posix_register_script(source_path: &str) -> Result<String, ApiError> {
    let metadata = remote_install_metadata_json(source_path, RemoteInstallPlatform::Posix)?;
    Ok(format!(
        r#"set -eu
SOURCE={source}
case "$SOURCE" in "~") SOURCE="$HOME" ;; "~/"*) SOURCE="$HOME/${{SOURCE#~/}}" ;; esac
if [ ! -d "$SOURCE" ]; then
  echo "TermAl checkout not found: $SOURCE" >&2
  exit 2
fi
if [ ! -f "$SOURCE/Cargo.toml" ]; then
  echo "Cargo.toml not found in TermAl checkout: $SOURCE" >&2
  exit 2
fi
if ! grep -Eq '^name[[:space:]]*=[[:space:]]*"termal"' "$SOURCE/Cargo.toml"; then
  echo "Cargo.toml at $SOURCE does not declare package name termal" >&2
  exit 2
fi
command -v git >/dev/null 2>&1 || {{ echo "git is not available on remote PATH" >&2; exit 2; }}
command -v cargo >/dev/null 2>&1 || {{ echo "cargo is not available on remote PATH" >&2; exit 2; }}
OS="$(uname -s 2>/dev/null || echo unknown)"
ARCH="$(uname -m 2>/dev/null || echo unknown)"
mkdir -p "$HOME/.termal/bin"
METADATA={metadata}
printf '%s\n' "$METADATA" > "$HOME/.termal/remote-install.json.tmp"
mv "$HOME/.termal/remote-install.json.tmp" "$HOME/.termal/remote-install.json"
printf 'registered TermAl checkout: %s\n' "$SOURCE"
printf 'remote platform: %s/%s\n' "$OS" "$ARCH"
"#,
        source = shell_quote(source_path),
        metadata = shell_quote(&metadata),
    ))
}

fn remote_windows_register_script(source_path: &str) -> String {
    format!(
        r#"$ErrorActionPreference = 'Stop'
$Source = {source}
if ($Source -eq '~') {{
  $Source = $HOME
}} elseif ($Source.StartsWith('~/') -or $Source.StartsWith('~\')) {{
  $Source = Join-Path $HOME $Source.Substring(2)
}}
if (-not (Test-Path -LiteralPath $Source -PathType Container)) {{
  [Console]::Error.WriteLine("TermAl checkout not found: $Source")
  exit 2
}}
if (-not (Test-Path -LiteralPath (Join-Path $Source 'Cargo.toml') -PathType Leaf)) {{
  [Console]::Error.WriteLine("Cargo.toml not found in TermAl checkout: $Source")
  exit 2
}}
$CargoToml = Get-Content -Raw -LiteralPath (Join-Path $Source 'Cargo.toml')
if ($CargoToml -notmatch '(?m)^name\s*=\s*"termal"') {{
  [Console]::Error.WriteLine("Cargo.toml at $Source does not declare package name termal")
  exit 2
}}
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {{
  [Console]::Error.WriteLine('git is not available on remote PATH')
  exit 2
}}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {{
  [Console]::Error.WriteLine('cargo is not available on remote PATH')
  exit 2
}}
$TermalDir = Join-Path $HOME '.termal'
$BinDir = Join-Path $TermalDir 'bin'
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$Metadata = @{{ sourcePath = $Source; platform = 'windows' }} | ConvertTo-Json -Compress
$Tmp = Join-Path $TermalDir 'remote-install.json.tmp'
$Meta = Join-Path $TermalDir 'remote-install.json'
Set-Content -LiteralPath $Tmp -Value $Metadata -Encoding UTF8
Move-Item -LiteralPath $Tmp -Destination $Meta -Force
Write-Output "registered TermAl checkout: $Source"
Write-Output "remote platform: windows/$env:PROCESSOR_ARCHITECTURE"
"#,
        source = powershell_quote(source_path),
    )
}

fn remote_posix_upgrade_script() -> String {
    r#"set -eu
META="$HOME/.termal/remote-install.json"
if [ ! -f "$META" ]; then
  echo "remote is not registered; run Register TermAl first" >&2
  exit 2
fi
PLATFORM="$(sed -n '@@PLATFORM_SED@@' "$META" | head -n 1)"
if [ "$PLATFORM" = "windows" ]; then
  echo "remote registration metadata targets Windows PowerShell lifecycle" >&2
  exit 2
fi
SOURCE="$(sed -n '@@SOURCE_PATH_SED@@' "$META" | head -n 1)"
if [ -z "$SOURCE" ]; then
  echo "remote registration metadata is missing sourcePath" >&2
  exit 2
fi
case "$SOURCE" in "~") SOURCE="$HOME" ;; "~/"*) SOURCE="$HOME/${SOURCE#~/}" ;; esac
if [ ! -d "$SOURCE" ]; then
  echo "registered TermAl checkout not found: $SOURCE" >&2
  exit 2
fi
LOG="$HOME/.termal/remote-upgrade.log"
mkdir -p "$HOME/.termal/bin"
(
  cd "$SOURCE"
  git pull --ff-only
  cargo build --release --bin termal
) >"$LOG" 2>&1 || {
  echo "remote build failed; last log lines:" >&2
  tail -80 "$LOG" >&2 || true
  exit 1
}
cp "$SOURCE/target/release/termal" "$HOME/.termal/bin/termal.new"
chmod +x "$HOME/.termal/bin/termal.new"
mv "$HOME/.termal/bin/termal.new" "$HOME/.termal/bin/termal"
"$HOME/.termal/bin/termal" --version
echo "updated TermAl from $SOURCE"
echo "build log: $LOG"
"#
    .replace("@@PLATFORM_SED@@", POSIX_METADATA_PLATFORM_SED)
    .replace("@@SOURCE_PATH_SED@@", POSIX_METADATA_SOURCE_PATH_SED)
}

fn remote_windows_upgrade_script() -> String {
    r#"$ErrorActionPreference = 'Stop'
$TermalDir = Join-Path $HOME '.termal'
$Meta = Join-Path $TermalDir 'remote-install.json'
if (-not (Test-Path -LiteralPath $Meta -PathType Leaf)) {
  [Console]::Error.WriteLine('remote is not registered; run Register TermAl first')
  exit 2
}
try {
  $Metadata = Get-Content -Raw -LiteralPath $Meta | ConvertFrom-Json
} catch {
  [Console]::Error.WriteLine("remote registration metadata is invalid: $_")
  exit 2
}
$Source = [string]$Metadata.sourcePath
if ([string]::IsNullOrWhiteSpace($Source)) {
  [Console]::Error.WriteLine('remote registration metadata is missing sourcePath')
  exit 2
}
if ($Source -eq '~') {
  $Source = $HOME
} elseif ($Source.StartsWith('~/') -or $Source.StartsWith('~\')) {
  $Source = Join-Path $HOME $Source.Substring(2)
}
if (-not (Test-Path -LiteralPath $Source -PathType Container)) {
  [Console]::Error.WriteLine("registered TermAl checkout not found: $Source")
  exit 2
}
$Log = Join-Path $TermalDir 'remote-upgrade.log'
$BinDir = Join-Path $TermalDir 'bin'
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$PushedLocation = $false
try {
  Push-Location -LiteralPath $Source
  $PushedLocation = $true
  git pull --ff-only *> $Log
  if ($LASTEXITCODE -ne 0) { throw 'git pull failed' }
  cargo build --release --bin termal *>> $Log
  if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
} catch {
  [Console]::Error.WriteLine('remote build failed; last log lines:')
  if (Test-Path -LiteralPath $Log -PathType Leaf) {
    Get-Content -LiteralPath $Log -Tail 80 | ForEach-Object { [Console]::Error.WriteLine($_) }
  }
  exit 1
} finally {
  if ($PushedLocation) {
    Pop-Location
  }
}
$Built = Join-Path $Source 'target\release\termal.exe'
$NewBinary = Join-Path $BinDir 'termal.exe.new'
$Binary = Join-Path $BinDir 'termal.exe'
Copy-Item -LiteralPath $Built -Destination $NewBinary -Force
Move-Item -LiteralPath $NewBinary -Destination $Binary -Force
& $Binary --version
Write-Output "updated TermAl from $Source"
Write-Output "build log: $Log"
"#
    .to_owned()
}

fn sanitize_remote_action_output(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut sanitized = text
        .chars()
        .filter_map(|ch| match ch {
            '\r' | '\n' | '\t' => Some(ch),
            _ if ch.is_control() => None,
            _ => Some(ch),
        })
        .collect::<String>();
    let trimmed_len = sanitized.trim_end().len();
    sanitized.truncate(trimmed_len);
    let mut chars = sanitized.chars();
    let mut limited = chars
        .by_ref()
        .take(MAX_REMOTE_ACTION_OUTPUT_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        let truncate_at = limited
            .char_indices()
            .nth(MAX_REMOTE_ACTION_OUTPUT_CHARS.saturating_sub(3))
            .map(|(index, _)| index)
            .unwrap_or(limited.len());
        limited.truncate(truncate_at);
        limited.push_str("...");
    }
    limited
}

struct RemoteScriptOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn read_capped_remote_action_stream<R: std::io::Read>(mut reader: R) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = MAX_REMOTE_ACTION_OUTPUT_BYTES.saturating_sub(output.len());
                if remaining > 0 {
                    output.extend_from_slice(&buffer[..read.min(remaining)]);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                eprintln!("remote action warning> failed to read SSH output stream: {err}");
                break;
            }
        }
    }
    output
}

fn join_remote_action_reader(handle: Option<std::thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    let Some(handle) = handle else {
        return Vec::new();
    };
    match handle.join() {
        Ok(output) => output,
        Err(err) => {
            eprintln!("remote action warning> SSH output reader thread panicked: {err:?}");
            Vec::new()
        }
    }
}

fn wait_for_remote_script(
    child: &mut Child,
    action: &str,
) -> Result<std::process::ExitStatus, ApiError> {
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(err) => {
                return Err(ApiError::bad_gateway(format!(
                    "failed to inspect remote {action} process: {err}"
                )));
            }
        }
        if started_at.elapsed() >= REMOTE_ACTION_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ApiError::bad_gateway(format!(
                "remote {action} timed out after {} seconds",
                REMOTE_ACTION_TIMEOUT.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn run_remote_ssh_script_child(
    remote: &RemoteConfig,
    action: &str,
    script: &RemoteActionScript,
) -> Result<RemoteScriptOutput, ApiError> {
    let mut child = Command::new("ssh")
        .args(script.command_args(remote)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| local_ssh_start_error(&remote.name, err))?;
    let stdout_handle = child
        .stdout
        .take()
        .map(|stdout| thread::spawn(move || read_capped_remote_action_stream(stdout)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stderr| thread::spawn(move || read_capped_remote_action_stream(stderr)));
    let status = wait_for_remote_script(&mut child, action)?;
    Ok(RemoteScriptOutput {
        status,
        stdout: join_remote_action_reader(stdout_handle),
        stderr: join_remote_action_reader(stderr_handle),
    })
}

fn remote_action_error(remote_name: &str, action: &str, output: &RemoteScriptOutput) -> ApiError {
    let stderr = sanitize_remote_action_output(&output.stderr);
    let stdout = sanitize_remote_action_output(&output.stdout);
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("ssh exited with {}", output.status)
    };
    ApiError::bad_gateway(format!(
        "remote `{remote_name}` {action} failed: {detail}"
    ))
}

fn run_remote_ssh_script(
    remote: &RemoteConfig,
    action: &str,
    script: &RemoteActionScript,
) -> Result<RemoteActionResponse, ApiError> {
    validate_remote_connection_config(remote)?;
    if remote.transport != RemoteTransport::Ssh {
        return Err(ApiError::bad_request(format!(
            "remote `{}` does not use SSH transport",
            remote.name
        )));
    }
    let output = run_remote_ssh_script_child(remote, action, script)?;
    if !output.status.success() {
        return Err(remote_action_error(&remote.name, action, &output));
    }
    Ok(RemoteActionResponse {
        remote_id: remote.id.clone(),
        action: action.to_owned(),
        message: format!("remote `{}` {action} completed", remote.name),
        stdout: sanitize_remote_action_output(&output.stdout),
        stderr: sanitize_remote_action_output(&output.stderr),
    })
}

fn remote_upgrade_should_try_windows(output: &RemoteScriptOutput) -> bool {
    remote_upgrade_should_try_windows_parts(output.status.code(), &output.stderr)
}

fn remote_upgrade_should_try_windows_parts(exit_code: Option<i32>, stderr: &[u8]) -> bool {
    if exit_code == Some(127) {
        return true;
    }
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    stderr.contains("remote registration metadata targets windows powershell lifecycle")
        || stderr.contains("sh: not found")
        || stderr.contains("sh: command not found")
        || stderr.contains("'sh' is not recognized")
}

fn upgrade_remote_ssh(remote: &RemoteConfig) -> Result<RemoteActionResponse, ApiError> {
    validate_remote_connection_config(remote)?;
    if remote.transport != RemoteTransport::Ssh {
        return Err(ApiError::bad_request(format!(
            "remote `{}` does not use SSH transport",
            remote.name
        )));
    }

    let posix_script = RemoteActionScript::Posix(remote_posix_upgrade_script());
    let posix_output = run_remote_ssh_script_child(remote, "upgrade", &posix_script)?;
    if posix_output.status.success() {
        return Ok(RemoteActionResponse {
            remote_id: remote.id.clone(),
            action: "upgrade".to_owned(),
            message: format!("remote `{}` upgrade completed", remote.name),
            stdout: sanitize_remote_action_output(&posix_output.stdout),
            stderr: sanitize_remote_action_output(&posix_output.stderr),
        });
    }

    if remote_upgrade_should_try_windows(&posix_output) {
        let windows_script = RemoteActionScript::WindowsPowerShell(remote_windows_upgrade_script());
        let windows_output = run_remote_ssh_script_child(remote, "upgrade", &windows_script)?;
        if windows_output.status.success() {
            return Ok(RemoteActionResponse {
                remote_id: remote.id.clone(),
                action: "upgrade".to_owned(),
                message: format!("remote `{}` upgrade completed", remote.name),
                stdout: sanitize_remote_action_output(&windows_output.stdout),
                stderr: sanitize_remote_action_output(&windows_output.stderr),
            });
        }
        return Err(remote_action_error(&remote.name, "upgrade", &windows_output));
    }

    Err(remote_action_error(&remote.name, "upgrade", &posix_output))
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
