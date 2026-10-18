// Owns fixed local identity/platform measurements in an isolated probe session.
// Does not execute peer commands, install tools or operate user sessions. New O1
// probes reuse bounded_read_process.rs for finite child ownership and reaping.

#[derive(Clone)]
struct FederationOnboardingIo {
    profile: Arc<dyn Fn() -> Result<FederationExecutionProfile, ApiError> + Send + Sync>,
    image: Arc<dyn Fn() -> FederationImageObservation + Send + Sync>,
    platform: Arc<dyn Fn() -> Result<FederationPlatformProbe, ApiError> + Send + Sync>,
    engram: Arc<
        dyn Fn(&FederationEngramProbeCommand) -> FederationEngramOnboardingOutcome + Send + Sync,
    >,
}

impl FederationOnboardingIo {
    fn local(directory: PathBuf) -> Self {
        Self::local_with_image(directory, capture_federation_process_image())
    }

    fn local_with_image(directory: PathBuf, image: Arc<FederationProcessImage>) -> Self {
        let probe_directory = directory.clone();
        let profile_image = image.clone();
        Self {
            profile: Arc::new(move || {
                federation_local_execution_profile(&directory, &profile_image)
            }),
            image: Arc::new(move || image.observe()),
            platform: Arc::new(move || federation_local_platform_probe(&probe_directory)),
            engram: Arc::new(run_federation_engram_probe),
        }
    }
}

fn federation_probe_command(
    program: &str,
    args: &[&str],
    directory: &FsPath,
) -> Result<String, ApiError> {
    #[cfg(windows)]
    let mut command = windows_launch::LaunchSpec::new(program);
    #[cfg(not(windows))]
    let mut command = Command::new(program);
    command.args(args).current_dir(directory);
    let output = run_bounded_read_command(
        &mut command,
        std::time::Instant::now() + Duration::from_secs(2),
        4096,
        true,
    )
    .map_err(|_| {
        ApiError::from_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "isolated platform probe could not spawn and reap its child",
        )
    })?;
    if !output.status.success() {
        return Err(ApiError::from_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "isolated platform probe child failed",
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| ApiError::bad_gateway("invalid platform probe output"))?;
    Ok(text.trim().to_owned())
}

fn federation_local_execution_profile(
    directory: &FsPath,
    image: &FederationProcessImage,
) -> Result<FederationExecutionProfile, ApiError> {
    let FederationImageObservation::Known(identity) = image.observe() else {
        return Err(ApiError::from_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "running image identity unknown",
        ));
    };
    let toolchain_identity = federation_probe_command("rustc", &["--version"], directory)?;
    Ok(FederationExecutionProfile {
        protocol_version: u32::from(FEDERATION_PROTOCOL_VERSION),
        schema_version: SQLITE_SCHEMA_VERSION.to_owned(),
        binary_sha256: identity.sha256,
        toolchain_identity,
    })
}

fn federation_local_platform_probe(
    directory: &FsPath,
) -> Result<FederationPlatformProbe, ApiError> {
    let directory = fs::canonicalize(directory).map_err(|_| {
        ApiError::from_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "isolated probe directory is unavailable",
        )
    })?;
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|_| ApiError::internal("isolated probe binary path is unavailable"))?;
    #[cfg(windows)]
    let (program, args, dialect) = (
        "cmd.exe",
        vec!["/d", "/s", "/c", "echo TERMAL_PROBE_WAKE"],
        "cmd",
    );
    #[cfg(not(windows))]
    let (program, args, dialect) = ("/bin/sh", vec!["-c", "printf TERMAL_PROBE_WAKE"], "posix");
    let output = federation_probe_command(program, &args, &directory)?;
    let toolchain_identity = federation_probe_command("rustc", &["--version"], &directory)?;
    Ok(FederationPlatformProbe {
        platform: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        shell_dialect: dialect.to_owned(),
        paths_resolved: executable.is_file() && directory.is_dir(),
        child_spawned_and_reaped: output == "TERMAL_PROBE_WAKE",
        toolchain_identity,
    })
}
