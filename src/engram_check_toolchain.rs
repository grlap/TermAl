// The toolchain label a check's environment evidence carries (Engram
// w-108a13d58018 criterion 2, tm-winf step 2). Owns the choice of checks
// whose toolchain TermAl can name without running anything the workspace
// chose, the rustup and cargo version probes, and their bounded output. Does
// not own when a label is captured or the environment fingerprint it feeds
// (`engram_turn_checks.rs`). Split out of `engram_turn_checks.rs`.

/// How long one toolchain command may take, within the checkpoint's shared
/// budget, before the check goes without a toolchain label.
const ENGRAM_TOOLCHAIN_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The toolchain selector of a check whose toolchain TermAl can name without
/// running anything the workspace chose: `Some(None)` for `cargo` run by name
/// without a selector, `Some(Some(name))` for `cargo +name`, and `None`
/// otherwise. A label is produced by version commands TermAl runs with its
/// own rights, outside any sandbox the check ran in, so it never runs a
/// program the workspace could have written or picked:
/// - not for a runner other than cargo: Python, Go and Node are commonly
///   started through version-manager shims that pick the interpreter from
///   files in the workspace, and can name a directory in it;
/// - not for a cargo run by a path, which is not the cargo TermAl would
///   find, and may be one the workspace holds;
/// - not for a selector that is not a plain toolchain name.
///
/// A `+` selector counts only as cargo's first argument, as rustup reads it.
fn engram_cargo_toolchain_selector(check: &EngramCheckCommand) -> Option<Option<String>> {
    if check.program != "cargo" {
        return None;
    }
    let words = engram_shell_words(engram_first_command(&check.normalized))?;
    if words[0].contains(['/', '\\', ':']) {
        return None;
    }
    match words.get(1).and_then(|word| word.strip_prefix('+')) {
        None => Some(None),
        Some(name) => engram_plain_toolchain_name(name).then(|| Some(name.to_owned())),
    }
}

/// Whether `name` can only name an installed toolchain, never a directory:
/// ASCII letters, digits, `.`, `_` and `-`, starting with a letter or digit.
fn engram_plain_toolchain_name(name: &str) -> bool {
    name.starts_with(|first: char| first.is_ascii_alphanumeric())
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

/// Whether the rustup that printed `version` (`rustup 1.29.1 (…)`) installs
/// nothing when it is only asked to show or locate a toolchain: 1.28 stopped
/// `rustup show` from installing and honours `RUSTUP_AUTO_INSTALL`.
fn engram_rustup_never_installs(version: &str) -> bool {
    version
        .strip_prefix("rustup ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|number| {
            let mut parts = number.split('.').map(str::parse::<u64>);
            Some((parts.next()?.ok()?, parts.next()?.ok()?))
        })
        .is_some_and(|version| version >= (1, 28))
}

/// The toolchain `rustup show active-toolchain` printed, when it is a plain
/// name (`stable-x86_64-pc-windows-msvc (default)`). A toolchain file can
/// name a directory instead, whose binaries the workspace may hold.
fn engram_rustup_active_toolchain(output: &str) -> Option<String> {
    let name = output.split_whitespace().next()?;
    engram_plain_toolchain_name(name).then(|| name.to_owned())
}

/// The program `name` resolves to on TermAl's own `PATH` (`search_path`),
/// when it lies outside the worktree with key `workspace`. The first match
/// decides, as it would for a shell: when that one is in the worktree, the
/// agent could have written it, so nothing is resolved. Relative entries are
/// skipped, since they would resolve against a directory TermAl did not
/// choose. On Windows a shell tries each directory's `PATHEXT` extensions in
/// order, so a `cargo.cmd` or `cargo.bat` shim ahead of a `cargo.exe` is what
/// runs; TermAl names only a plain `.exe`, and nothing when a shim comes
/// first.
fn engram_host_program(
    search_path: &std::ffi::OsStr,
    name: &str,
    workspace: &str,
) -> Option<PathBuf> {
    // Windows looks a name up without regard to case; PATHEXT lists the
    // extensions in upper case, and the files usually carry them in lower.
    let extensions = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    } else {
        vec![String::new()]
    };
    let program = std::env::split_paths(search_path)
        .filter(|directory| directory.is_absolute())
        .find_map(|directory| {
            extensions
                .iter()
                .map(|extension| directory.join(format!("{name}{extension}")))
                .find(|candidate| engram_is_executable_file(candidate))
        })?;
    if cfg!(windows)
        && !program
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return None;
    }
    (!engram_path_within(&engram_canonical_path_key(&program), workspace)).then_some(program)
}

fn engram_is_executable_file(path: &FsPath) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// The toolchain label of the check `command` run in `directory`, whose
/// toolchain overrides chose what it ran with: the first line of the
/// toolchain's `rustc -V` and `cargo -V`, joined, within Engram's bound.
/// Taken as the check starts (`EngramTurnCheck::toolchain`). `None` when
/// TermAl cannot name the toolchain safely
/// (`engram_cargo_toolchain_selector`), when a command fails, or when
/// `deadline` passes first, so the check goes without environment evidence
/// rather than with a guessed one.
fn engram_toolchain_label(
    command: &EngramCheckCommand,
    directory: &FsPath,
    deadline: std::time::Instant,
) -> Option<String> {
    let selector = engram_cargo_toolchain_selector(command)?;
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    engram_cargo_toolchain_label(
        &search_path,
        selector.as_deref(),
        directory,
        &|program, args, directory| engram_probe_output(program, args, directory, deadline),
    )
}

/// Runs a version command: `program` with `args` in a directory, giving the
/// first line it prints (`engram_probe_output`).
type EngramToolchainProbe<'a> = &'a dyn Fn(&FsPath, &[&str], &FsPath) -> Option<String>;

/// The label of the cargo toolchain `selector` names, or with none the one
/// active in `workdir`, the check's directory. The cargo a check run by name
/// runs is the first on the `PATH`. When that is rustup's own proxy (rustup
/// lies beside it), rustup names the toolchain (it reads the workspace's
/// toolchain file and overrides without running them) and where its `rustc`
/// and `cargo` are; each must lie outside the worktree, and is then run by
/// that path. Otherwise that cargo and the `rustc` beside it are labelled
/// themselves, and a `+` selector names nothing. Every version command runs
/// through `probe`, in the directory of the program it runs, never in the
/// workspace, but for rustup's `show active-toolchain`, which reads the
/// overrides of the check's directory.
fn engram_cargo_toolchain_label(
    search_path: &std::ffi::OsStr,
    selector: Option<&str>,
    workdir: &FsPath,
    probe: EngramToolchainProbe<'_>,
) -> Option<String> {
    let workspace = engram_worktree_root(workdir);
    let cargo = engram_host_program(search_path, "cargo", &workspace)?;
    let rustup = engram_host_program(search_path, "rustup", &workspace)
        .filter(|rustup| rustup.parent() == cargo.parent());
    let tools = match rustup {
        Some(rustup) => {
            let rustup_directory = rustup.parent()?;
            // An older rustup ignores RUSTUP_AUTO_INSTALL and may install the
            // toolchain a workspace file names while it is only asked to show
            // it; such a rustup is not asked at all.
            if !engram_rustup_never_installs(&probe(&rustup, &["--version"], rustup_directory)?) {
                return None;
            }
            let name = match selector {
                Some(name) => name.to_owned(),
                None => engram_rustup_active_toolchain(&probe(
                    &rustup,
                    &["show", "active-toolchain"],
                    workdir,
                )?)?,
            };
            let tool = |tool: &str| {
                probe(
                    &rustup,
                    &["which", "--toolchain", &name, tool],
                    rustup_directory,
                )
                .map(PathBuf::from)
                .filter(|path| {
                    path.is_absolute()
                        && !engram_path_within(&engram_canonical_path_key(path), &workspace)
                })
            };
            [tool("rustc")?, tool("cargo")?]
        }
        // A cargo that is not rustup's proxy is labelled with the rustc
        // beside it; a `+` selector names nothing for it.
        None if selector.is_none() => [
            engram_host_program(search_path, "rustc", &workspace)
                .filter(|rustc| rustc.parent() == cargo.parent())?,
            cargo,
        ],
        None => return None,
    };
    let versions = tools
        .iter()
        .map(|tool| probe(tool, &["-V"], tool.parent()?))
        .collect::<Option<Vec<_>>>()?;
    Some(
        engram_truncate_utf8(
            versions.join("; ").trim(),
            ENGRAM_ENVIRONMENT_LABEL_MAX_BYTES,
        )
        .to_owned(),
    )
}

/// At most this much of a version command's output is kept.
const ENGRAM_TOOLCHAIN_PROBE_OUTPUT_MAX_BYTES: usize = 64 * 1024;

/// The first non-empty line `program` with `args` prints in `directory`, if
/// it succeeds by the earlier of `deadline` and its own bound. It runs through
/// the shared bounded reader, owning its whole process tree (a Windows job
/// without a console window, a Unix process group), so a timeout or an early
/// exit ends every process it started. Rustup's automatic installs are
/// off, so a probe downloads nothing; output past the bound fails the probe.
fn engram_probe_output(
    program: &FsPath,
    args: &[&str],
    directory: &FsPath,
    deadline: std::time::Instant,
) -> Option<String> {
    let deadline = deadline.min(std::time::Instant::now() + ENGRAM_TOOLCHAIN_PROBE_TIMEOUT);
    if std::time::Instant::now() >= deadline {
        return None;
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(directory)
        .env("RUSTUP_AUTO_INSTALL", "0");
    // Owning the tree matters on Unix too: a probe runs straight from the
    // host, not inside a checker process whose group ends with it.
    let output = run_bounded_read_process(
        &mut command,
        deadline,
        ENGRAM_TOOLCHAIN_PROBE_OUTPUT_MAX_BYTES,
        true,
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}
