// Owns platform handle admission for Telegram documents, split from
// telegram_document_files.rs. Does not own roots, wire types or uploads.
// A checked pathname is never reopened for reading: path and regular-file
// admission are checked on the handle that supplies every uploaded byte.

fn telegram_document_file_error(path: &FsPath, error: io::Error) -> TelegramDocumentError {
    let detail = format!("cannot read {}: {error}", path.display());
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ELOOP) {
        return TelegramDocumentError::PathOutsideAllowedRoots(detail);
    }
    #[cfg(windows)]
    if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_INVALID_NAME as i32) {
        return TelegramDocumentError::InvalidRequest(detail);
    }
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => {
            TelegramDocumentError::FileNotFound(detail)
        }
        io::ErrorKind::InvalidInput => TelegramDocumentError::InvalidRequest(detail),
        _ => TelegramDocumentError::Internal(detail),
    }
}

fn refuse_telegram_document_namespace(path: &FsPath) -> Result<(), TelegramDocumentError> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(prefix)) = path.components().next() {
            // Device/UNC names must not cause pipe/SMB I/O before admission.
            // Verbatim disk paths are local drive paths, as canonicalize emits.
            if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
                return Err(TelegramDocumentError::PathOutsideAllowedRoots(
                    "network and device document paths are not allowed".to_owned(),
                ));
            }
            if !path.is_absolute() {
                return Err(TelegramDocumentError::InvalidRequest(
                    "drive-relative document paths are not allowed".to_owned(),
                ));
            }
        }
    }
    let _ = path;
    Ok(())
}

fn open_telegram_document(path: &FsPath) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(path)
}

fn telegram_document_is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn validate_telegram_document_handle(
    file: &fs::File,
    expected: &FsPath,
    roots: &[PathBuf],
    after_observation: impl FnOnce(&FsPath),
) -> Result<(), TelegramDocumentError> {
    let actual = telegram_document_handle_path(file, after_observation)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| telegram_document_file_error(expected, err))?;
    if actual != expected || !roots.iter().any(|root| actual.starts_with(root)) {
        return Err(TelegramDocumentError::PathOutsideAllowedRoots(
            "document path changed or left the allowed roots while opening or reading".to_owned(),
        ));
    }
    Ok(())
}

fn telegram_document_handle_path(
    file: &fs::File,
    after_observation: impl FnOnce(&FsPath),
) -> io::Result<PathBuf> {
    let path = telegram_document_raw_handle_path(file)?;
    after_observation(&path);
    // The kernel's observation must not be walked by name again. A concurrent
    // rename/symlink swap could make that lookup name a different inode.
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().ends_with(b" (deleted)") {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "document handle no longer has an absolute live path",
        ));
    }
    Ok(path)
}

#[cfg(windows)]
fn telegram_document_raw_handle_path(file: &fs::File) -> io::Result<PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    // Windows paths are bounded by the extended-path limit. The call reports
    // the required size instead of overrunning this initialized buffer.
    let mut buffer = vec![0u16; 32768];
    // SAFETY: the live borrowed handle and writable buffer outlive this call;
    // the API receives the buffer's actual capacity in UTF-16 code units.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "document handle path is too long",
        ));
    }
    Ok(PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..length as usize],
    )))
}

#[cfg(target_os = "linux")]
fn telegram_document_raw_handle_path(file: &fs::File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "macos")]
fn telegram_document_raw_handle_path(file: &fs::File) -> io::Result<PathBuf> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes into this initialized
    // buffer, and file retains ownership of the valid descriptor throughout.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let length = buffer.iter().position(|byte| *byte == 0).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "unterminated document handle path",
        )
    })?;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(
        buffer[..length].to_vec(),
    )))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn telegram_document_raw_handle_path(_file: &fs::File) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "document handle containment is unavailable",
    ))
}
