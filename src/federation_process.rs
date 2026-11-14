// Owns a single federation SSH child and reap-before-replace cleanup.
// Does not start a remote TermAl server. New sibling to remote_ssh.rs.

struct FederationSshProcess {
    child: Option<Child>,
    input: Option<std::process::ChildStdin>,
    output: Option<std::process::ChildStdout>,
    diagnostic: FederationSshDiagnostics,
}

impl FederationSshProcess {
    #[cfg(test)]
    fn from_child(child: Child) -> Result<Self, ApiError> {
        let diagnostic = Arc::new(Mutex::new(FederationSshDiagnostic::default()));
        let mut process = Self::retain_child(child, diagnostic);
        process.attach_stdio();
        Ok(process)
    }

    fn retain_child(child: Child, diagnostic: FederationSshDiagnostics) -> Self {
        Self {
            child: Some(child),
            input: None,
            output: None,
            diagnostic,
        }
    }

    fn attach_stdio(&mut self) {
        let child = self.child.as_mut().expect("owned SSH child before stdio setup");
        self.input = child.stdin.take();
        self.output = child.stdout.take();
        let stderr = child.stderr.take();
        let diagnostic = self.diagnostic.clone();
        if let Some(stderr) = stderr {
            if std::thread::Builder::new()
                .name("federation-stderr".to_owned())
                .spawn(move || federation_read_ssh_diagnostics(stderr, &diagnostic))
                .is_err() {
                    // The child already exists. Return its owned guard even
                    // when diagnostic-thread creation fails, so cleanup stays
                    // with the worker rather than becoming a no-child error.
                    eprintln!("federation SSH diagnostic reader unavailable");
                }
        }
    }

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
            Some(child) => {
                let status = child
                    .try_wait()
                    .map_err(|_| federation_transport_unavailable())?;
                if let Some(status) = &status {
                    self.diagnostic
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .exit(status);
                }
                Ok(status.is_some())
            }
            None => Ok(true),
        }
    }

    // Called on spawn_blocking, and completed before another attempt starts.
    #[cfg(test)]
    fn shutdown(mut self) -> Result<(), ApiError> {
        self.reap()
    }

    fn reap(&mut self) -> Result<(), ApiError> {
        if let Some(child) = self.child.as_mut() {
            let status = child.try_wait();
            if let Ok(Some(status)) = &status {
                self.diagnostic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .exit(status);
            } else {
                self.diagnostic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .cleanup_requested = true;
                if let Err(error) = child.kill() {
                    eprintln!("federation SSH cleanup could not stop owned child: {error}");
                }
            }
            let status = child.wait().map_err(|error| {
                eprintln!("federation SSH cleanup could not reap owned child: {error}");
                ApiError::internal(
                    "federation SSH cleanup pending; retaining owned child for retry",
                )
                .with_kind(ApiErrorKind::FederationSshCleanupUnknown)
            })?;
            self.diagnostic
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .exit(&status);
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
    owned: &mut Option<Box<dyn FederationSshChild>>,
) -> Result<(), ApiError> {
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
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    // Allocate diagnostics before spawn. Once spawn returns, publish the exact
    // guard before stdio setup, thread creation or other post-spawn work.
    let diagnostic = Arc::new(Mutex::new(FederationSshDiagnostic::default()));
    let child = command
        .spawn()
        .map_err(|_| federation_transport_unavailable())?;
    *owned = Some(Box::new(FederationSshProcess::retain_child(child, diagnostic)));
    owned.as_mut().expect("published SSH guard").prepare_control()
}
