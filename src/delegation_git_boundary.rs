// Git metadata validation for delegation approval and provider worktree placement.
// Owns bounded reads and same-repository linked-worktree validation.

use std::{
    fs, io,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

#[cfg(windows)]
use super::normalize_user_facing_path;
use super::{
    FederationImageFileIdentity, engram_network_path, federation_image_file_identity,
    identity_open, normalize_path_best_effort, review_freeze_is_reparse, review_freeze_same_entry,
    test_run_dir_within_bounds,
};

pub(super) fn is_valid(directory: &FsPath, marker_is_dir: bool) -> io::Result<bool> {
    let marker = directory.join(".git");
    let git_dir = if marker_is_dir {
        marker.clone()
    } else {
        let Some(line) = metadata_line(&marker)? else {
            return Ok(false);
        };
        let Some(path) = line
            .strip_prefix("gitdir: ")
            .filter(|path| !path.trim().is_empty())
        else {
            return Ok(false);
        };
        directory.join(path)
    };
    if !fs::metadata(&git_dir)?.is_dir() || !head_is_valid(&git_dir)? {
        return Ok(false);
    }

    // Linked worktrees keep objects and refs in commondir, and their gitdir
    // backpointer must identify this marker. A broken link grants no exemption.
    let common_dir = match fs::metadata(git_dir.join("commondir")) {
        Ok(metadata) if metadata.is_file() => {
            let Some(common) = metadata_line(&git_dir.join("commondir"))? else {
                return Ok(false);
            };
            let Some(backpointer) = metadata_line(&git_dir.join("gitdir"))? else {
                return Ok(false);
            };
            if normalize_path_best_effort(&git_dir.join(backpointer))
                != normalize_path_best_effort(&marker)
            {
                return Ok(false);
            }
            git_dir.join(common)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if git_dir.join("gitdir").try_exists()? {
                return Ok(false);
            }
            git_dir.clone()
        }
        Err(err) => return Err(err),
        Ok(_) => return Ok(false),
    };
    Ok(head_is_valid(&common_dir)?
        && fs::metadata(common_dir.join("objects"))?.is_dir()
        && fs::metadata(common_dir.join("refs"))?.is_dir())
}

pub(super) fn metadata_line(path: &FsPath) -> io::Result<Option<String>> {
    use std::io::Read as _;
    let inspected = fs::symlink_metadata(path)?;
    if !inspected.is_file()
        || inspected.file_type().is_symlink()
        || review_freeze_is_reparse(&inspected)
    {
        return Ok(None);
    }
    // The shared opener uses O_NONBLOCK/O_NOFOLLOW on Unix, so a FIFO or
    // link substituted after inspection cannot make this request wait.
    let file = identity_open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.file_type().is_symlink() || review_freeze_is_reparse(&opened) {
        return Ok(None);
    }
    let mut text = String::new();
    file.take(8193).read_to_string(&mut text)?;
    let line = text.trim();
    Ok(
        (text.len() <= 8192 && !line.is_empty() && !line.contains(['\r', '\n', '\0']))
            .then(|| line.to_owned()),
    )
}

fn head_is_valid(git_dir: &FsPath) -> io::Result<bool> {
    let Some(head) = metadata_line(&git_dir.join("HEAD"))? else {
        return Ok(false);
    };
    Ok(valid_head(&head))
}

fn valid_head(head: &str) -> bool {
    head.strip_prefix("ref: ").is_some_and(|reference| {
        reference.starts_with("refs/")
            && !reference.contains("..")
            && !reference.chars().any(char::is_whitespace)
    }) || (matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// A directory used for provider placement. Every child is
/// opened without following links. Unix resolves the name relative to the
/// opened directory; Windows also holds its path components against renames.
struct ProviderDirectory {
    path: PathBuf,
    directory: Arc<fs::File>,
    #[cfg(windows)]
    ancestors: Vec<Arc<fs::File>>,
}

impl ProviderDirectory {
    fn open(path: &FsPath) -> io::Result<Self> {
        use std::path::Component;
        let path = provider_local_path(path).ok_or_else(invalid_provider_layout)?;
        let mut components = path.components().peekable();
        let mut root = PathBuf::new();
        while matches!(
            components.peek(),
            Some(Component::Prefix(_) | Component::RootDir)
        ) {
            root.push(components.next().expect("a root component").as_os_str());
        }
        #[cfg(unix)]
        let directory = identity_open(&root)?;
        #[cfg(windows)]
        let directory = open_provider_path_windows(&root, true)?;
        let mut result = Self {
            path: root,
            directory: Arc::new(directory),
            #[cfg(windows)]
            ancestors: Vec::new(),
        };
        for component in components {
            let Component::Normal(name) = component else {
                return Err(invalid_provider_layout());
            };
            result = result.child(name)?;
        }
        Ok(result)
    }

    fn child(&self, name: &std::ffi::OsStr) -> io::Result<Self> {
        let directory = self.open_child(name, true)?;
        Ok(Self {
            path: self.path.join(name),
            directory: Arc::new(directory),
            #[cfg(windows)]
            ancestors: self
                .ancestors
                .iter()
                .chain([&self.directory])
                .cloned()
                .collect(),
        })
    }

    fn line(&self, name: &str) -> io::Result<String> {
        use std::io::Read as _;
        const LIMIT: usize = 8192;
        let mut file = self.open_child(name.as_ref(), false)?;
        let before = file.metadata()?;
        if before.len() > LIMIT as u64 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "provider file exceeds its read limit",
            ));
        }
        let mut bytes = Vec::new();
        (&mut file).take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > LIMIT
            || before.len() != bytes.len() as u64
            || !review_freeze_same_entry(&before, &file.metadata()?)
            || self.file_identity(&file)?
                != self.file_identity(&self.open_child(name.as_ref(), false)?)?
        {
            return Err(invalid_provider_layout());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid_provider_layout())?;
        let line = text.trim();
        if line.is_empty() || line.contains(['\r', '\n', '\0']) {
            return Err(invalid_provider_layout());
        }
        Ok(line.to_owned())
    }

    fn file_identity(&self, file: &fs::File) -> io::Result<FederationImageFileIdentity> {
        federation_image_file_identity(file, &file.metadata()?)
    }

    fn open_child(&self, name: &std::ffi::OsStr, directory: bool) -> io::Result<fs::File> {
        let mut components = FsPath::new(name).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(part)) if part == name)
            || components.next().is_some()
            || (cfg!(windows) && name.to_string_lossy().contains(':'))
        {
            return Err(invalid_provider_layout());
        }
        #[cfg(unix)]
        let file = open_provider_child_unix(&self.directory, name, directory)?;
        #[cfg(windows)]
        let file = open_provider_path_windows(&self.path.join(name), directory)?;
        let metadata = file.metadata()?;
        if metadata.file_type().is_symlink()
            || review_freeze_is_reparse(&metadata)
            || (directory && !metadata.is_dir())
            || (!directory && !metadata.is_file())
        {
            return Err(invalid_provider_layout());
        }
        Ok(file)
    }
}

#[cfg(unix)]
fn open_provider_child_unix(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    directory: bool,
) -> io::Result<fs::File> {
    use std::os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, FromRawFd},
    };
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| invalid_provider_layout())?;
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    let flags = if directory {
        flags | libc::O_DIRECTORY
    } else {
        flags
    };
    // SAFETY: the name is NUL-terminated and the borrowed parent stays open.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor owned by this function.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_provider_path_windows(path: &FsPath, directory: bool) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    if directory {
        // Pin the component against rename/delete while permitting writes
        // and atomic replacements of its children.
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    options.open(path)
}

fn invalid_provider_layout() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "provider Git directory or file changed or is unsupported",
    )
}

fn provider_local_path(path: &FsPath) -> Option<PathBuf> {
    if !path.is_absolute()
        || engram_network_path(path.to_str()?)
        || !test_run_dir_within_bounds(path)
        || path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return None;
    }
    // Use the physical form of these well-known system aliases for access.
    // User-facing paths can retain the familiar names.
    #[cfg(target_os = "macos")]
    for prefix in ["/tmp", "/var", "/etc"] {
        if let Ok(rest) = path.strip_prefix(prefix) {
            return Some(FsPath::new("/private").join(&prefix[1..]).join(rest));
        }
    }
    #[cfg(windows)]
    return Some(normalize_user_facing_path(path));
    #[cfg(not(windows))]
    Some(path.to_path_buf())
}

/// Locate a registered linked worktree of the session's own repository.
pub(super) struct ProviderLinkedWorktree {
    pub(super) root: PathBuf,
    pub(super) admin_identity: FederationImageFileIdentity,
}

impl ProviderLinkedWorktree {
    pub(super) fn resolve(session: &FsPath, cwd: &FsPath) -> io::Result<Self> {
        let (session, session_marker) = find_provider_worktree(session)?;
        let common = match session_marker {
            ProviderGitMarker::Directory(common) => common,
            ProviderGitMarker::Pointer(pointer) => {
                let admin = provider_git_pointer(&session, &pointer)?;
                let common = linked_provider_common(&admin)?;
                let common = ProviderDirectory::open(common)?;
                open_provider_linked_admin(&session, &admin, &common)?;
                common
            }
        };
        if !valid_head(&common.line("HEAD")?) {
            return Err(invalid_provider_layout());
        }
        common.child("objects".as_ref())?;
        common.child("refs".as_ref())?;

        let (root, marker) = find_provider_worktree(cwd)?;
        let ProviderGitMarker::Pointer(pointer) = marker else {
            return Err(invalid_provider_layout());
        };
        let admin_path = provider_git_pointer(&root, &pointer)?;
        let admin = open_provider_linked_admin(&root, &admin_path, &common)?;
        let admin_identity = admin.file_identity(&admin.directory)?;
        Ok(Self {
            root,
            admin_identity,
        })
    }

    pub(super) fn artifact_parent(
        root: &FsPath,
        identity: &FederationImageFileIdentity,
    ) -> io::Result<PathBuf> {
        let root = provider_local_path(root).ok_or_else(invalid_provider_layout)?;
        let pointer = ProviderDirectory::open(&root)?.line(".git")?;
        let admin_path = provider_git_pointer(&root, &pointer)?;
        let common = ProviderDirectory::open(linked_provider_common(&admin_path)?)?;
        let admin = open_provider_linked_admin(&root, &admin_path, &common)?;
        if admin.file_identity(&admin.directory)? != *identity {
            return Err(invalid_provider_layout());
        }
        Ok(admin.path.join("review-runs"))
    }
}

enum ProviderGitMarker {
    Directory(ProviderDirectory),
    Pointer(String),
}

fn find_provider_worktree(cwd: &FsPath) -> io::Result<(PathBuf, ProviderGitMarker)> {
    let cwd = ProviderDirectory::open(cwd)?;
    for path in cwd.path.ancestors() {
        let directory = ProviderDirectory::open(path)?;
        if let Ok(marker) = directory.child(".git".as_ref()) {
            return Ok((path.to_path_buf(), ProviderGitMarker::Directory(marker)));
        }
        match directory.line(".git") {
            Ok(line) => return Ok((path.to_path_buf(), ProviderGitMarker::Pointer(line))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(invalid_provider_layout())
}

fn provider_git_pointer(root: &FsPath, line: &str) -> io::Result<PathBuf> {
    let path = line
        .strip_prefix("gitdir: ")
        .filter(|path| !path.is_empty())
        .ok_or_else(invalid_provider_layout)?;
    resolve_provider_pointer(root, path)
}

fn resolve_provider_pointer(base: &FsPath, value: &str) -> io::Result<PathBuf> {
    use std::path::Component;
    if engram_network_path(value) {
        return Err(invalid_provider_layout());
    }
    let mut path = PathBuf::new();
    for component in base.join(value).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Check the prefix before stepping up. Use the opened
                // spelling so an admitted system alias keeps its real parent.
                path = ProviderDirectory::open(&path)?.path;
                if !path.pop() {
                    return Err(invalid_provider_layout());
                }
            }
            _ => path.push(component.as_os_str()),
        }
    }
    provider_local_path(&path).ok_or_else(invalid_provider_layout)
}

fn linked_provider_common(admin: &FsPath) -> io::Result<&FsPath> {
    admin
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "worktrees"))
        .and_then(FsPath::parent)
        .ok_or_else(invalid_provider_layout)
}

fn open_provider_linked_admin(
    root: &FsPath,
    admin_path: &FsPath,
    common: &ProviderDirectory,
) -> io::Result<ProviderDirectory> {
    if linked_provider_common(admin_path)? != common.path {
        return Err(invalid_provider_layout());
    }
    let name = admin_path.file_name().ok_or_else(invalid_provider_layout)?;
    let admin = common.child("worktrees".as_ref())?.child(name)?;
    if resolve_provider_pointer(&admin.path, &admin.line("commondir")?)? != common.path
        || resolve_provider_pointer(&admin.path, &admin.line("gitdir")?)? != root.join(".git")
        || !valid_head(&admin.line("HEAD")?)
    {
        return Err(invalid_provider_layout());
    }
    Ok(admin)
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn provider_git_pointer_parent_uses_the_physical_system_alias() {
    let base = FsPath::new("/");
    let native = fs::canonicalize("/tmp/..").unwrap();
    assert_eq!(native, FsPath::new("/private"));
    assert_eq!(provider_git_pointer(base, "gitdir: /tmp/..").unwrap(), native);
}
