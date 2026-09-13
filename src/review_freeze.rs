// New compiled read-only verifier for the Engram schema-1 freeze protocol.
// Owns hashing and manifest admission, not reviewer lifecycle or arbitrary
// script execution. Wire algorithm matches Engram's review-freeze-fingerprint
// schema 1; the implementation is host-owned, never loaded from the target tree.

#[cfg(windows)]
const REVIEW_FREEZE_WINDOWS_LIMITATION: &str = "Review freeze limitation: untracked executable-mode and filesystem symlink properties are unverified on Windows; Git index modes and symlink targets are covered separately.\n";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewFreezeRequest {
    manifest_path: String,
    expected_fingerprint: String,
}

impl ReviewFreezeRequest {
    fn validate(&self) -> Result<()> {
        if self.manifest_path.is_empty()
            || self.manifest_path.len() > 4096
            || self.manifest_path.chars().any(char::is_control)
            || !is_lowercase_sha256(&self.expected_fingerprint)
        {
            bail!("expected a bounded manifest path and independent lowercase SHA-256 fingerprint");
        }
        Ok(())
    }
}

fn review_freeze_mode(args: &[String]) -> Result<()> {
    if args.len() != 4 {
        bail!("usage: termal review-freeze-check ROOT MANIFEST EXPECTED_SHA256");
    }
    let request = ReviewFreezeRequest {
        manifest_path: args[2].clone(),
        expected_fingerprint: args[3].clone(),
    };
    let fingerprint = check_review_freeze(FsPath::new(&args[1]), &request)?;
    #[cfg(windows)]
    eprint!("{REVIEW_FREEZE_WINDOWS_LIMITATION}");
    println!("{fingerprint}");
    Ok(())
}

fn review_freeze_chunk(hash: &mut Sha256, label: &str, bytes: &[u8]) {
    hash.update((label.len() as u64).to_be_bytes());
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(label.as_bytes());
    hash.update(bytes);
}

struct ReviewFreezeGit {
    root: PathBuf,
    binary: PathBuf,
    deadline: std::time::Instant,
}

impl ReviewFreezeGit {
    fn new(cwd: &FsPath) -> Result<Self> {
        let root = fs::canonicalize(cwd)?;
        // Never allow Windows' current-directory executable search, a relative
        // PATH entry, or a git shim supplied by the reviewed repository.
        let executable = if cfg!(windows) { "git.exe" } else { "git" };
        let binary = std::env::split_paths(&std::env::var_os("PATH").context("PATH missing")?)
            .filter(|dir| dir.is_absolute())
            .filter_map(|dir| fs::canonicalize(dir.join(executable)).ok())
            .find(|path| path.is_file() && !path.starts_with(&root))
            .context("trusted Git executable outside the reviewed tree not found")?;
        let git = Self {
            root,
            binary,
            deadline: std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT,
        };
        let top = git.run(&["rev-parse", "--show-toplevel"], false)?;
        if fs::canonicalize(String::from_utf8(top)?.trim())? != git.root {
            bail!("review cwd must be the worktree root");
        }
        // Git diff can invoke a clean/process filter even with --no-ext-diff.
        // Reject such repositories rather than running their configured code.
        let filters = git.run(
            &[
                "config",
                "--null",
                "--get-regexp",
                r"^filter\..*\.(clean|smudge|process)$",
            ],
            true,
        )?;
        if !filters.is_empty() {
            bail!("review verification does not run configured Git filters");
        }
        Ok(git)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(name);
            }
        }
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        command
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            // Missing promisor objects must fail locally. Also deny all
            // transports, independently of repository protocol/helper config.
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .args([
                "--no-pager",
                "-c",
                "core.fsmonitor=false",
                "-c",
                &format!("core.hooksPath={null}"),
                "-c",
                "diff.submodule=short",
            ])
            // Unset config still falls back to XDG/HOME ignore/attributes.
            // Pin these paths as well; repository .gitignore/.gitattributes
            // remain inputs. A differently normalized parent must re-freeze.
            .args([
                "-c",
                &format!("core.excludesFile={null}"),
                "-c",
                &format!("core.attributesFile={null}"),
            ])
            .args(args);
        command
    }

    fn run(&self, args: &[&str], allow_absent: bool) -> Result<Vec<u8>> {
        let mut command = self.command(args);
        let output = run_bounded_read_process(
            &mut command,
            self.deadline,
            REVIEW_FREEZE_OUTPUT_LIMIT,
            false,
        )?;
        if !output.status.success() && !(allow_absent && output.status.code() == Some(1)) {
            bail!(
                "Git verification {} failed: {}",
                args[0],
                String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(4096)])
            );
        }
        Ok(output.stdout)
    }
}

fn review_freeze_file(path: &FsPath, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() > limit as u64 {
        bail!("unsupported or oversized review input");
    }
    let mut file = fs::File::open(path)?;
    let opened = file.metadata()?;
    if !review_freeze_same_entry(&before, &opened) {
        bail!("review input changed while opening");
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit
        || bytes.len() as u64 != before.len()
        || !review_freeze_same_entry(&before, &file.metadata()?)
        || !review_freeze_same_entry(&before, &fs::symlink_metadata(path)?)
    {
        bail!("review input changed while reading");
    }
    Ok(bytes)
}

fn review_freeze_same_entry(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if a.dev() != b.dev()
            || a.ino() != b.ino()
            || a.mode() != b.mode()
            || a.ctime() != b.ctime()
            || a.ctime_nsec() != b.ctime_nsec()
        {
            return false;
        }
    }
    a.len() == b.len()
        && a.modified().ok() == b.modified().ok()
        && a.created().ok() == b.created().ok()
        && a.file_type() == b.file_type()
}

fn review_freeze_untracked_path(root: &FsPath, path: &str) -> Result<PathBuf> {
    if path.ends_with('/') {
        bail!("embedded untracked Git repositories are unsupported by review verification");
    }
    if path.is_empty()
        || path.contains('\\')
        || path.contains(':')
        || FsPath::new(path).is_absolute()
        || path
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        bail!("unsafe untracked path");
    }
    let full = root.join(path);
    // A final symlink is hashed as a link; a symlink/reparse-point ancestor is
    // not followed outside the worktree (or into another worktree subtree).
    let parent = full.parent().context("untracked path has no parent")?;
    if fs::canonicalize(parent)? != parent {
        bail!("untracked ancestor redirects outside its lexical path");
    }
    Ok(full)
}

fn capture_review_freeze(git: &ReviewFreezeGit) -> Result<String> {
    let head = git.run(&["rev-parse", "--verify", "--quiet", "HEAD"], true)?;
    let head = String::from_utf8(head)?;
    let head = if head.trim().is_empty() {
        "UNBORN"
    } else {
        head.trim()
    };
    // A diff can run status (and clean/process filters) in submodules using
    // their own config, outside the superproject filter check. Inspect only
    // index/tree metadata, never submodule worktrees. Reject both added and
    // removed gitlinks; an unborn index may also contain one.
    let index = git.run(&["ls-files", "--stage", "-z"], false)?;
    let tree = if head == "UNBORN" {
        Vec::new()
    } else {
        git.run(&["ls-tree", "-r", "-z", head], false)?
    };
    if [&index, &tree].iter().any(|entries| {
        entries
            .split(|b| *b == 0)
            .any(|entry| entry.starts_with(b"160000 "))
    }) {
        bail!("review verification does not inspect submodules (Git gitlinks are unsupported)");
    }
    let mut hash = Sha256::new();
    review_freeze_chunk(&mut hash, "schema", b"1");
    review_freeze_chunk(&mut hash, "head", head.as_bytes());
    for (label, args) in [
        (
            "staged-diff",
            vec![
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=all",
            ],
        ),
        (
            "unstaged-diff",
            vec![
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=all",
            ],
        ),
    ] {
        review_freeze_chunk(&mut hash, label, &git.run(&args, false)?);
    }
    let paths = git.run(&["ls-files", "--others", "--exclude-standard", "-z"], false)?;
    let mut paths = paths
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();
    if paths.len() > 20_000 {
        bail!("too many untracked review inputs");
    }
    paths.sort();
    let mut total = 0usize;
    for bytes in paths {
        if std::time::Instant::now() >= git.deadline {
            bail!("review verification deadline exceeded");
        }
        let path = std::str::from_utf8(bytes)
            .context("non-UTF8 untracked path is unsupported by Engram v1")?;
        let full = review_freeze_untracked_path(&git.root, path)?;
        let metadata = fs::symlink_metadata(&full)?;
        if metadata.file_type().is_symlink() {
            review_freeze_chunk(&mut hash, &format!("untracked-path:{path}"), b"symlink");
            let target = fs::read_link(&full)?;
            let target = target.to_str().context("non-UTF8 symlink target")?;
            review_freeze_chunk(
                &mut hash,
                &format!("untracked-target:{path}"),
                target.as_bytes(),
            );
            if !review_freeze_same_entry(&metadata, &fs::symlink_metadata(&full)?) {
                bail!("symlink changed");
            }
        } else {
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            review_freeze_chunk(
                &mut hash,
                &format!("untracked-mode:{path}"),
                if executable { b"100755" } else { b"100644" },
            );
            let content = review_freeze_file(&full, REVIEW_FREEZE_OUTPUT_LIMIT)?;
            total += content.len();
            if total > 512 * 1024 * 1024 {
                bail!("untracked review input budget exceeded");
            }
            review_freeze_chunk(&mut hash, &format!("untracked-content:{path}"), &content);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn check_review_freeze(cwd: &FsPath, request: &ReviewFreezeRequest) -> Result<String> {
    request.validate()?;
    let git = ReviewFreezeGit::new(cwd)?;
    let manifest_path = FsPath::new(&request.manifest_path);
    if manifest_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("manifest path cannot contain parent traversal");
    }
    let manifest_path = if manifest_path.is_absolute() {
        manifest_path.to_path_buf()
    } else {
        git.root.join(manifest_path)
    };
    let manifest_path = fs::canonicalize(manifest_path)?;
    if !manifest_path.starts_with(&git.root) {
        bail!("manifest must be inside the review worktree");
    }
    let manifest_bytes = review_freeze_file(&manifest_path, 1024 * 1024)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    let manifest_root = manifest
        .get("root")
        .and_then(Value::as_str)
        .context("manifest root missing")?;
    if manifest.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        || !FsPath::new(manifest_root).is_absolute()
        || fs::canonicalize(manifest_root)? != git.root
        || manifest.get("fingerprint").and_then(Value::as_str)
            != Some(request.expected_fingerprint.as_str())
    {
        bail!("review manifest schema/root/independent fingerprint mismatch");
    }
    // Two captures and a manifest re-read reject concurrent drift. They are
    // stability checks, not a filesystem snapshot or proof of no transient edits.
    let first = capture_review_freeze(&git)?;
    if first != request.expected_fingerprint
        || capture_review_freeze(&git)? != first
        || review_freeze_file(&manifest_path, 1024 * 1024)? != manifest_bytes
    {
        bail!("review input drifted from the independently supplied fingerprint");
    }
    Ok(first)
}
