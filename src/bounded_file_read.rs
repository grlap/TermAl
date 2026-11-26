// Bounded same-entry file reads and reparse detection for run artifacts and
// delegation paths. The file opening and identity checks come from the former
// schema-1 checker.

use super::*;

/// Read bounded bytes and the metadata of the one file version they
/// came from: the path, the opened handle and the path again after the
/// read all showed that same entry, so the metadata belongs to these bytes
/// and not to an earlier or later replacement.
pub(crate) fn read_bounded_file_versioned(
    path: &FsPath,
    limit: usize,
) -> Result<(Vec<u8>, fs::Metadata)> {
    let (bytes, metadata, ()) = read_bounded_file_observed(path, limit, |_, _| Ok(()))?;
    Ok((bytes, metadata))
}

/// Observe the same opened file that supplies the checked bytes.
fn read_bounded_file_observed<T>(
    path: &FsPath,
    limit: usize,
    observe: impl FnOnce(&fs::File, &fs::Metadata) -> Result<T>,
) -> Result<(Vec<u8>, fs::Metadata, T)> {
    use std::io::Read;
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() > limit as u64 {
        bail!("unsupported or oversized review input");
    }
    #[cfg(test)]
    test_bounded_file_before_open(path);
    let mut file = match open_file_without_waiting(path, &before) {
        Ok(file) => file,
        // A link put in place after the inspection: the entry changed.
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            bail!("review input changed while opening")
        }
        Err(error) => return Err(error.into()),
    };
    let opened = file.metadata()?;
    if file_reparse_appeared(&before, &opened) || !same_file_entry(&before, &opened) {
        bail!("review input changed while opening");
    }
    let observation = observe(&file, &opened)?;
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit
        || bytes.len() as u64 != before.len()
        || !same_file_entry(&before, &file.metadata()?)
        // An entry gone or unreadable after the read changed while it was
        // read; it is not an absent input.
        || !fs::symlink_metadata(path).is_ok_and(|after| same_file_entry(&before, &after))
    {
        bail!("review input changed while reading");
    }
    Ok((bytes, before, observation))
}

/// Opens for reading without following a final link and, on Unix, without
/// waiting: an entry replaced by a link or a FIFO after it was inspected is
/// refused or opened at once, and the handle's metadata then refuses it. On
/// Windows an entry inspected as an ordinary file is opened without following
/// a reparse point (FILE_FLAG_OPEN_REPARSE_POINT), so a link put in place
/// shows as a reparse point in the handle's metadata; an entry that already
/// was a non-link reparse file (deduplicated or a cloud placeholder) is opened
/// as before, since opening its reparse point would not read its data. There
/// are no FIFOs in the Windows namespace.
fn open_file_without_waiting(path: &FsPath, inspected: &fs::Metadata) -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A link shows as ELOOP here, whatever the inspection saw.
        let _ = inspected;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        if file_metadata_is_reparse(inspected) {
            return fs::File::open(path);
        }
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = inspected;
        fs::File::open(path)
    }
}

/// Whether metadata carries a Windows reparse point of any kind. Always false
/// elsewhere, where links show as symlinks.
pub(crate) fn file_metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

/// The opened handle is a link, or a reparse point where the inspection saw
/// none: the entry was replaced between the two.
fn file_reparse_appeared(inspected: &fs::Metadata, opened: &fs::Metadata) -> bool {
    opened.file_type().is_symlink()
        || (file_metadata_is_reparse(opened) && !file_metadata_is_reparse(inspected))
}

/// Whether metadata read without following describes a link of any kind,
/// including a Windows reparse point (a junction or other) that std does not
/// report as a symlink.
pub(crate) fn file_metadata_is_link(metadata: &fs::Metadata) -> bool {
    file_metadata_is_reparse(metadata) || metadata.file_type().is_symlink()
}

/// A test may replace the entry at a path ending in the registered suffix
/// between its inspection and its open, as a racing writer would
/// (`install_test_bounded_file_before_open`). The suffix keeps the hook
/// independent of how the caller spelled the directory.
#[cfg(test)]
static TEST_BOUNDED_FILE_BEFORE_OPEN: Mutex<Vec<(PathBuf, Box<dyn FnOnce() + Send>)>> =
    Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn install_test_bounded_file_before_open(
    path: PathBuf,
    replace: impl FnOnce() + Send + 'static,
) {
    TEST_BOUNDED_FILE_BEFORE_OPEN
        .lock()
        .expect("test hook mutex poisoned")
        .push((path, Box::new(replace)));
}

#[cfg(test)]
fn test_bounded_file_before_open(path: &FsPath) {
    let hook = {
        let mut hooks = TEST_BOUNDED_FILE_BEFORE_OPEN
            .lock()
            .expect("test hook mutex poisoned");
        hooks
            .iter()
            .position(|(suffix, _)| path.ends_with(suffix))
            .map(|index| hooks.remove(index).1)
    };
    if let Some(replace) = hook {
        replace();
    }
}

fn same_file_entry(a: &fs::Metadata, b: &fs::Metadata) -> bool {
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
