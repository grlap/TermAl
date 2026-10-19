//! Private Git fixtures and actual-executable transport for identity tests.
//! Owns no production identity implementation or live host/store access.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

pub struct Fixture {
    pub home: PathBuf,
    pub root: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let project = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        let scratch = project.join(".tmp");
        fs::create_dir_all(&scratch).unwrap();
        assert!(
            !fs::symlink_metadata(&scratch)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::canonicalize(&scratch).unwrap(), scratch);
        let home = scratch.join(format!("identity-cli-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&home).unwrap();
        let root = home.join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--object-format=sha1"], None);
        Self { home, root }
    }

    pub fn put(&self, path: &str, bytes: &[u8]) {
        put(&self.root, path, bytes);
    }

    pub fn commit(&self) {
        commit(&self.root);
    }

    pub fn capture(&self) -> Value {
        successful(&self.root, json!({"operation":"capture","schemaVersion":1}))
    }

    pub fn check(&self, fingerprint: &str) -> Value {
        successful(
            &self.root,
            json!({
                "operation":"check", "schemaVersion":1,
                "expectedFingerprint":fingerprint, "platformProfile":platform()
            }),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only this UUID fixture is removed; no live run/worktree is a target.
        let project = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        let resolved = fs::canonicalize(&self.home).unwrap();
        assert!(resolved.starts_with(project.join(".tmp")));
        let cleanup = (|| {
            prepare_fixture_removal(&resolved, &resolved)?;
            // Recheck the absolute target after the walk. Native removal
            // unlinks nested symlinks; it never follows their targets.
            if fs::canonicalize(&self.home)? != resolved {
                return Err(std::io::Error::other("fixture removal target changed"));
            }
            fs::remove_dir_all(&resolved)
        })();
        if let Err(error) = cleanup {
            if std::thread::panicking() {
                eprintln!("identity fixture cleanup {}: {error}", resolved.display());
            } else {
                panic!("identity fixture cleanup {}: {error}", resolved.display());
            }
        }
    }
}

fn prepare_fixture_removal(path: &Path, home: &Path) -> std::io::Result<()> {
    if !path.starts_with(home) {
        return Err(std::io::Error::other("fixture removal escaped its owner"));
    }
    let metadata = fs::symlink_metadata(path)?;
    let link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt;
        link || metadata.file_attributes() & 0x400 != 0
    };
    if link {
        return Ok(());
    }
    if metadata.is_dir() {
        if fs::canonicalize(path)? != path {
            return Err(std::io::Error::other("fixture directory redirected"));
        }
        for entry in fs::read_dir(path)? {
            prepare_fixture_removal(&entry?.path(), home)?;
        }
    }
    #[cfg(windows)]
    {
        if metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

pub fn platform() -> &'static str {
    if cfg!(windows) { "windows" } else { "unix" }
}

pub fn put(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

pub fn git(root: &Path, args: &[&str], input: Option<&[u8]>) -> String {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Identity Fixture")
        .env("GIT_AUTHOR_EMAIL", "identity@example.test")
        .env("GIT_COMMITTER_NAME", "Identity Fixture")
        .env("GIT_COMMITTER_EMAIL", "identity@example.test")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .args([
            "--no-pager",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            &format!("core.hooksPath={null}"),
            "-c",
            &format!("init.templateDir={null}"),
        ])
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = input {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

pub fn commit(root: &Path) {
    git(root, &["add", "--", "."], None);
    git(root, &["commit", "--quiet", "-m", "fixture"], None);
}

pub fn child(owner: &Path, owner_git: &Path, path: &str) -> (PathBuf, PathBuf) {
    let worktree = owner.join(path);
    let gitdir = owner_git.join("modules").join(path);
    fs::create_dir_all(gitdir.parent().unwrap()).unwrap();
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        owner,
        &[
            "init",
            "--object-format=sha1",
            "--separate-git-dir",
            &git_path(&gitdir),
            &git_path(&worktree),
        ],
        None,
    );
    put(&worktree, "tracked", b"child\n");
    commit(&worktree);
    let oid = git(&worktree, &["rev-parse", "HEAD"], None);
    git(
        owner,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},{path}"),
        ],
        None,
    );
    (worktree, gitdir)
}

fn git_path(path: &Path) -> String {
    let text = path.to_str().unwrap();
    if cfg!(windows) {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(drive) = text.strip_prefix(r"\\?\") {
            return drive.to_owned();
        }
    }
    text.to_owned()
}

pub fn invoke(root: &Path, request: Value) -> Output {
    Command::new(env!("CARGO_BIN_EXE_termal"))
        .arg("review-identity")
        .arg(root)
        .arg(serde_json::to_string(&request).unwrap())
        .current_dir(root)
        .output()
        .unwrap()
}

pub fn successful(root: &Path, request: Value) -> Value {
    let output = invoke(root, request);
    assert!(
        output.status.success(),
        "identity CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).expect("one complete JSON result");
    assert_eq!(result["schemaVersion"], 1);
    assert_eq!(result["algorithm"], "termal-review-identity-v1");
    assert_eq!(result["platformProfile"], platform());
    assert!(matches!(
        result["objectFormat"].as_str(),
        Some("sha1" | "sha256")
    ));
    assert_eq!(result["complete"], true);
    let hash = result["fingerprint"].as_str().unwrap();
    assert_eq!(hash.len(), 64);
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(
        Path::new(result["canonicalRoot"].as_str().unwrap()),
        fs::canonicalize(root).unwrap()
    );
    result
}

pub fn refused(root: &Path, request: Value, code: &str) -> Value {
    let output = invoke(root, request);
    assert!(
        !output.status.success(),
        "refused identity unexpectedly succeeded"
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "expected structured identity refusal, got stdout={:?}, stderr={:?}",
            output.stdout,
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(result["verified"], false);
    assert_eq!(result["complete"], false);
    assert_eq!(result["reason"]["code"], code);
    assert!(
        result.get("fingerprint").is_none_or(Value::is_null),
        "no usable fallback fingerprint"
    );
    result
}

pub fn entries(result: &Value) -> &[Value] {
    result["inventory"]["entries"].as_array().unwrap()
}

pub fn entry<'a>(result: &'a Value, path: &str, layer: &str) -> &'a Value {
    entries(result)
        .iter()
        .find(|e| e["path"] == path && e["layer"] == layer)
        .unwrap_or_else(|| panic!("missing {layer} entry {path}: {result}"))
}
