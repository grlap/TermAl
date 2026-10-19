// Raw filesystem observations for review identity. Never follows a reviewed
// link; regular reads compare the opened handle, path identity and metadata.
fn identity_utf8_path(path: &FsPath) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| identity_refusal("path_encoding_unsupported"))
}
fn identity_plain_metadata(metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(identity_refusal("git_metadata_escape"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(identity_refusal("git_metadata_escape"));
        }
    }
    Ok(())
}
fn identity_no_links(path: &FsPath) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if component == std::path::Component::ParentDir {
            current.pop();
            continue;
        }
        current.push(component);
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        if current.as_os_str().is_empty() {
            continue;
        }
        identity_plain_metadata(&fs::symlink_metadata(&current)?)?;
    }
    Ok(())
}
fn identity_safe_path(root: &FsPath, relative: &str) -> Result<PathBuf> {
    identity_relative(relative.as_bytes())?;
    let full = root.join(relative);
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.split('/').collect();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                // Classify the entry itself without following it, including
                // when it is the final candidate before a child .git probe.
                // Final candidates retain their kind refusal. Tracked paths
                // below an unwalked link retain the ancestor-link contract.
                #[cfg(windows)]
                if index + 1 < components.len() {
                    identity_reparse_kind(&current, &metadata)
                        .map_err(|_| identity_refusal("ancestor_link"))?;
                } else {
                    identity_reparse_kind(&current, &metadata)?;
                }
                if index + 1 < components.len() {
                    identity_plain_metadata(&metadata)
                        .map_err(|_| identity_refusal("ancestor_link"))?;
                    if !metadata.is_dir() {
                        return Err(identity_refusal("ancestor_link"));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(full)
}
fn identity_exact_spelling(git: &ReviewIdentityGit, relative: &str) -> Result<bool> {
    let mut parent = git.root.clone();
    for component in relative.split('/') {
        let mut found = false;
        for entry in fs::read_dir(&parent)? {
            git.check_deadline()?;
            if entry?.file_name() == std::ffi::OsStr::new(component) {
                found = true;
                break;
            }
        }
        if !found {
            return Ok(false);
        }
        parent.push(component);
        if parent != git.root.join(relative) {
            identity_plain_metadata(&fs::symlink_metadata(&parent)?)?;
        }
    }
    Ok(true)
}
fn identity_open(path: &FsPath) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the entry itself, including directories, never its target.
        options.custom_flags(0x00200000 | 0x02000000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}
fn identity_physical_key(path: &FsPath) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        Ok(format!("unix:{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let file = identity_open(path)?;
        let metadata = file.metadata()?;
        Ok(format!(
            "{:?}",
            federation_image_file_identity(&file, &metadata)?
        ))
    }
}
fn identity_read_file(path: &FsPath, limit: usize) -> Result<(Vec<u8>, fs::Metadata)> {
    use std::io::Read;
    let before = fs::symlink_metadata(path)?;
    identity_plain_metadata(&before)?;
    if !before.is_file() {
        return Err(identity_refusal("special_file"));
    }
    if before.len() > limit as u64 {
        return Err(identity_refusal("input_limit_exceeded"));
    }
    let mut file = identity_open(path)?;
    let opened = file.metadata()?;
    let id = federation_image_file_identity(&file, &opened)?;
    if !review_freeze_same_entry(&before, &opened) {
        return Err(identity_refusal("concurrent_change"));
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    let after = fs::symlink_metadata(path)?;
    let path_handle = identity_open(path)?;
    if bytes.len() > limit
        || bytes.len() as u64 != opened.len()
        || !review_freeze_same_entry(&opened, &file.metadata()?)
        || !review_freeze_same_entry(&opened, &after)
        || id != federation_image_file_identity(&path_handle, &path_handle.metadata()?)?
    {
        return Err(identity_refusal("concurrent_change"));
    }
    Ok((bytes, opened))
}

fn identity_hash_file(
    path: &FsPath,
    deadline: std::time::Instant,
    capture: &mut ReviewIdentityCapture,
) -> Result<(u64, String, fs::Metadata)> {
    use std::io::Read;
    let before = fs::symlink_metadata(path)?;
    identity_plain_metadata(&before)?;
    if !before.is_file() {
        return Err(identity_refusal("special_file"));
    }
    if before.len() > REVIEW_IDENTITY_PAYLOAD_LIMIT as u64 {
        return Err(identity_refusal("input_limit_exceeded"));
    }
    let mut file = identity_open(path)?;
    let opened = file.metadata()?;
    let id = federation_image_file_identity(&file, &opened)?;
    if !review_freeze_same_entry(&before, &opened) {
        return Err(identity_refusal("concurrent_change"));
    }
    let mut buffer = [0u8; 64 * 1024];
    let mut hash = Sha256::new();
    let mut length = 0u64;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(identity_refusal("deadline_exceeded"));
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        if length > REVIEW_IDENTITY_PAYLOAD_LIMIT as u64 {
            return Err(identity_refusal("input_limit_exceeded"));
        }
        capture.consume(count)?;
        hash.update(&buffer[..count]);
    }
    let path_handle = identity_open(path)?;
    if length != opened.len()
        || !review_freeze_same_entry(&opened, &file.metadata()?)
        || !review_freeze_same_entry(&opened, &fs::symlink_metadata(path)?)
        || id != federation_image_file_identity(&path_handle, &path_handle.metadata()?)?
    {
        return Err(identity_refusal("concurrent_change"));
    }
    Ok((length, format!("{:x}", hash.finalize()), opened))
}

#[cfg(windows)]
fn identity_reparse_kind(path: &FsPath, metadata: &fs::Metadata) -> Result<Option<&'static str>> {
    use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo, GetFileInformationByHandleEx,
    };
    if metadata.file_attributes() & 0x400 == 0 {
        return Ok(None);
    }
    let file = identity_open(path)?;
    let mut info: FILE_ATTRIBUTE_TAG_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the borrowed handle and initialized POD outlive the synchronous
    // metadata query; the API receives exactly the structure's byte size.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    match info.ReparseTag {
        0xA000000C => Ok(Some("symlink")),
        0xA0000003 => Err(identity_refusal("link_kind_unsupported")),
        _ => Err(identity_refusal("special_file")),
    }
}
fn identity_worktree_entry(
    git: &ReviewIdentityGit,
    path: &str,
    prefix: &str,
    capture: &mut ReviewIdentityCapture,
) -> Result<()> {
    git.check_deadline()?;
    let full = identity_safe_path(&git.root, path)?;
    let metadata = match fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return capture.add(ReviewIdentityEntry::new(
                identity_join(prefix, path),
                "worktree",
                "absent",
            ));
        }
        Err(e) => return Err(e.into()),
    };
    #[cfg(windows)]
    let symlink = identity_reparse_kind(&full, &metadata)? == Some("symlink");
    #[cfg(not(windows))]
    let symlink = metadata.file_type().is_symlink();
    let mut entry = ReviewIdentityEntry::new(identity_join(prefix, path), "worktree", "regular");
    if symlink {
        entry.kind = "symlink".to_owned();
        entry.target = Some(identity_utf8_path(&fs::read_link(&full)?)?);
        let after = fs::symlink_metadata(&full)?;
        if !review_freeze_same_entry(&metadata, &after) {
            return Err(identity_refusal("concurrent_change"));
        }
    } else if metadata.is_file() {
        let (size, hash, metadata) = identity_hash_file(&full, git.runner.deadline, capture)?;
        entry.size = Some(size);
        entry.content_sha256 = Some(hash);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            entry.fs_mode = Some(json!({"unix":metadata.permissions().mode() & 0o7777}));
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            entry.fs_mode = Some(json!({"windows":null}));
        }
    } else if metadata.is_dir() {
        // A tracked path changed to a directory: include its boundary; Git's
        // nonignored untracked list separately enumerates all contained bytes.
        entry.kind = "directory-boundary".to_owned();
    } else {
        return Err(identity_refusal("special_file"));
    }
    capture.add(entry)
}
