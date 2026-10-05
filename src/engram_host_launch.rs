// Host-owned Engram processes have no reason to retain a project's worktree.
// This launch policy is runtime-private; durable intents keep their original
// project/store/session authority and acquire the policy at the adapter boundary.
fn engram_host_workdir(persistence_path: &FsPath) -> std::io::Result<PathBuf> {
    let absolute = if persistence_path.is_absolute() {
        persistence_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(persistence_path)
    };
    absolute
        .parent()
        .map(FsPath::to_path_buf)
        .ok_or_else(|| std::io::Error::other("TermAl persistence path has no parent directory"))
}

#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
enum EngramHostProgramKind {
    Native,
    PowerShell,
    Batch,
}

#[cfg(windows)]
fn engram_host_program_kind(binary: &FsPath) -> EngramHostProgramKind {
    match binary.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("ps1") => EngramHostProgramKind::PowerShell,
        Some(extension) if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") => EngramHostProgramKind::Batch,
        _ => EngramHostProgramKind::Native,
    }
}

#[cfg(windows)]
fn resolve_engram_host_program(
    binary: &FsPath,
    project_root: &FsPath,
) -> Result<PathBuf, EngramTransportError> {
    use std::os::windows::ffi::OsStringExt;

    let configured_kind = engram_host_program_kind(binary);
    let explicit_wrapper = configured_kind != EngramHostProgramKind::Native;
    let candidate = if binary.is_absolute() {
        binary.to_path_buf()
    } else if binary.components().count() > 1 {
        if !project_root.is_absolute() {
            return Err(EngramTransportError::spawn_failed(
                "Engram project root must be absolute",
            ));
        }
        project_root.join(binary)
    } else if explicit_wrapper {
        // A configured script keeps its explicit interpreter path. Do not add
        // PATHEXT candidates to a native command name or to this script name.
        let local = project_root.join(binary);
        if local.is_file() {
            local
        } else {
            std::env::var_os("PATH")
                .and_then(|value| std::env::split_paths(&value)
                    .map(|directory| directory.join(binary))
                    .find(|candidate| candidate.is_file()))
                .ok_or_else(|| EngramTransportError::spawn_failed(format!(
                    "Engram script not found: {}", binary.display()
                )))?
        }
    } else {
        // Share the native Windows search used by finite reads, modeled on
        // std::process::Command: application/system directories then PATH,
        // with .exe for an extensionless name. A PATH batch shim cannot win.
        let wide = windows_launch::resolve_program_path(&windows_launch::LaunchSpec::new(binary))
            .map_err(|error| EngramTransportError::spawn_failed(format!(
                "failed resolving Engram program {}: {error}", binary.display()
            )))?;
        PathBuf::from(std::ffi::OsString::from_wide(&wide[..wide.len() - 1]))
    };
    // Resolve before selecting the child cwd, including relative PATH entries.
    let canonical = fs::canonicalize(&candidate).map_err(|error| {
        EngramTransportError::spawn_failed(format!(
            "failed resolving Engram program {}: {error}",
            candidate.display()
        ))
    })?;
    let resolved_kind = engram_host_program_kind(&canonical);
    if configured_kind != resolved_kind {
        return Err(EngramTransportError::spawn_failed(format!(
            "Engram launch kind changed from {configured_kind:?} at {} to {resolved_kind:?} at {}; no process was started",
            binary.display(), canonical.display()
        )));
    }
    // PowerShell interprets a verbatim local script path as a remote path for
    // execution policy. Preserve ordinary spelling only after verifying that
    // it names the identical canonical file; never relax execution policy.
    let ordinary = normalize_user_facing_path(&canonical);
    if fs::canonicalize(&ordinary).is_ok_and(|resolved| resolved == canonical) {
        Ok(ordinary)
    } else {
        Ok(canonical)
    }
}

#[cfg(windows)]
fn validate_engram_host_launch_paths(
    project_file: &FsPath,
    home: &FsPath,
    host_workdir: &FsPath,
) -> Result<(), EngramTransportError> {
    if !project_file.is_absolute() || !home.is_absolute() {
        return Err(EngramTransportError::local_state(
            "Engram project file and home must be absolute",
        ));
    }
    if !host_workdir.is_absolute() || !host_workdir.is_dir() {
        return Err(EngramTransportError::local_state(
            "Engram host cwd must be an existing absolute persistence directory",
        ));
    }
    Ok(())
}

fn engram_host_command(
    binary: &FsPath,
    project_root: &FsPath,
    project_file: &FsPath,
    home: &FsPath,
    host_workdir: &FsPath,
) -> Result<Command, EngramTransportError> {
    #[cfg(windows)]
    {
        validate_engram_host_launch_paths(project_file, home, host_workdir)?;
        let binary = resolve_engram_host_program(binary, project_root)?;
        engram_host_command_for_program(&binary, project_root, host_workdir)
    }
    #[cfg(not(windows))]
    {
        let _ = (project_file, home, host_workdir);
        let mut command = engram_command(binary);
        command.current_dir(project_root);
        Ok(command)
    }
}

#[cfg(windows)]
fn engram_host_command_for_program(
    binary: &FsPath,
    project_root: &FsPath,
    host_workdir: &FsPath,
) -> Result<Command, EngramTransportError> {
    let extension = binary
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let mut command = if extension.eq_ignore_ascii_case("ps1") {
        let mut command = Command::new(resolve_engram_host_program(
            FsPath::new("powershell.exe"),
            project_root,
        )?);
        command
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(binary);
        command
    } else if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
        let mut command = Command::new(resolve_engram_host_program(
            FsPath::new("cmd.exe"),
            project_root,
        )?);
        command.args(["/D", "/S", "/C"]).arg(binary);
        command
    } else {
        Command::new(binary)
    };
    command.current_dir(host_workdir);
    Ok(command)
}

impl AppState {
    fn engram_host_launch_workdir(&self) -> PathBuf {
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .engram_host_adapter
            .host_workdir
            .clone()
    }
}
