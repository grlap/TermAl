// Owns a single federation SSH child and reap-before-replace cleanup.
// Does not start a remote TermAl server. New sibling to remote_ssh.rs.

struct FederationSshProcess {
    child: Option<Child>,
    input: Option<std::process::ChildStdin>,
    output: Option<std::process::ChildStdout>,
}

impl FederationSshProcess {
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        let input = self
            .input
            .take()
            .ok_or_else(federation_transport_unavailable)?;
        let output = self
            .output
            .take()
            .ok_or_else(federation_transport_unavailable)?;
        federation_stdio_control(input, output)
    }
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        match self.child.as_mut() {
            Some(child) => child
                .try_wait()
                .map(|status| status.is_some())
                .map_err(|_| federation_transport_unavailable()),
            None => Ok(true),
        }
    }

    // Called on spawn_blocking, and completed before another attempt starts.
    fn shutdown(mut self) -> Result<(), ApiError> {
        self.reap()
    }

    fn reap(&mut self) -> Result<(), ApiError> {
        if let Some(child) = self.child.as_mut() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                if let Err(error) = child.kill() {
                    eprintln!("federation SSH cleanup could not stop owned child: {error}");
                }
            }
            if let Err(error) = child.wait() {
                eprintln!("federation SSH cleanup could not reap owned child: {error}");
                return Err(ApiError::internal(
                    "federation SSH cleanup unresolved; operator reconciliation required",
                )
                .with_kind(ApiErrorKind::FederationSshCleanupUnknown));
            }
            self.child.take();
        }
        Ok(())
    }
}

impl Drop for FederationSshProcess {
    fn drop(&mut self) {
        // Unwind fallback only; normal shutdown is explicitly off the async
        // executor and awaited. A dropped Child alone would leave SSH alive.
        if let Err(error) = self.reap() {
            eprintln!("{}", error.message);
        }
    }
}

fn start_federation_ssh(
    snapshot: &FederationPeerSnapshot,
    forward_port: u16,
    known_hosts: &FsPath,
) -> Result<FederationSshProcess, ApiError> {
    if snapshot.local_role != FederationRole::Central {
        return Err(federation_stale_link());
    }
    validate_federation_reference(&snapshot.endpoint.host_key_ref)?;
    let remote = RemoteConfig {
        id: snapshot.peer.host_id.clone(),
        name: "Satellite host".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some(snapshot.endpoint.ssh_host.clone()),
        port: Some(snapshot.endpoint.ssh_port),
        user: Some(snapshot.endpoint.ssh_user.clone()),
    };
    let forwards = FederationForwards {
        local_forward_port: forward_port,
        satellite_listener_port: snapshot.endpoint.satellite_federation_port,
        reverse_forward_port: snapshot.endpoint.satellite_reverse_forward_port,
        central_listener_port: snapshot.endpoint.central_federation_port,
        host_key_alias: snapshot.endpoint.host_key_ref.clone(),
    };
    let args = federation_ssh_command_args(&remote, &forwards, known_hosts)?;
    let mut command = Command::new("ssh");
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command
        .spawn()
        .map_err(|_| federation_transport_unavailable())?;
    let input = child.stdin.take();
    let output = child.stdout.take();
    Ok(FederationSshProcess {
        child: Some(child),
        input,
        output,
    })
}
