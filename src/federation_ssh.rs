// Owns federation-only SSH argv, not enrollment, process lifetimes or APIs.
// New transport beside remote_ssh.rs; reuses its endpoint validation seam.

struct FederationForwards {
    local_forward_port: u16,
    satellite_listener_port: u16,
    reverse_forward_port: u16,
    central_listener_port: u16,
    host_key_alias: String,
}

fn federation_ssh_command_args(
    remote: &RemoteConfig,
    forwards: &FederationForwards,
    known_hosts: &FsPath,
) -> Result<Vec<String>, ApiError> {
    validate_remote_connection_config(remote)?;
    if remote.transport != RemoteTransport::Ssh
        || remote.port == Some(0)
        || [
            forwards.local_forward_port,
            forwards.satellite_listener_port,
            forwards.reverse_forward_port,
            forwards.central_listener_port,
        ]
        .contains(&0)
    {
        return Err(ApiError::bad_request(
            "invalid federation SSH configuration",
        ));
    }
    validate_federation_reference(&forwards.host_key_alias)?;
    let path = known_hosts
        .to_str()
        .ok_or_else(|| ApiError::bad_request("invalid known-hosts path"))?;
    if path.trim().is_empty()
        || path
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\'' | '"' | '%' | '~' | '$'))
        || path.starts_with("\\\\")
        || path.starts_with("//")
    {
        return Err(ApiError::bad_request("invalid known-hosts path"));
    }
    #[cfg(windows)]
    let path = path.replace('\\', "/");
    // OpenSSH parses the value of -o itself. Quote whitespace within that
    // one argv value, without invoking a shell or expanding path tokens.
    let path = if path.chars().any(char::is_whitespace) {
        format!("\"{path}\"")
    } else {
        path.to_owned()
    };
    let mut args: Vec<String> = ["-F", "none", "-N", "-T", "-a", "-x"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ExitOnForwardFailure=yes",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
        "GatewayPorts=no",
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "GlobalKnownHostsFile=none",
        "UpdateHostKeys=no",
        "VerifyHostKeyDNS=no",
        "CheckHostIP=no",
        "PermitLocalCommand=no",
        "ProxyCommand=none",
        "ProxyJump=none",
    ] {
        args.extend(["-o".to_owned(), option.to_owned()]);
    }
    args.extend([
        "-o".to_owned(),
        format!("UserKnownHostsFile={path}"),
        "-o".to_owned(),
        format!("HostKeyAlias={}", forwards.host_key_alias),
        "-p".to_owned(),
        remote.port.unwrap_or(22).to_string(),
        "-L".to_owned(),
        format!(
            "127.0.0.1:{}:127.0.0.1:{}",
            forwards.local_forward_port, forwards.satellite_listener_port
        ),
        "-R".to_owned(),
        format!(
            "127.0.0.1:{}:127.0.0.1:{}",
            forwards.reverse_forward_port, forwards.central_listener_port
        ),
        "--".to_owned(),
        remote_ssh_target(remote)?,
    ]);
    Ok(args)
}
