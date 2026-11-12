// Approval Git-boundary validation, introduced from delegations.rs.
// Owns bounded, regular-file reads of Git metadata and linked-worktree layout.
// Does not choose delegation scopes, grant approvals or manage delegation lifecycle.

use std::{fs, io, path::Path as FsPath};

use super::{identity_open, normalize_path_best_effort, review_freeze_is_reparse};

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

fn metadata_line(path: &FsPath) -> io::Result<Option<String>> {
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
    Ok(head.strip_prefix("ref: ").is_some_and(|reference| {
        reference.starts_with("refs/")
            && !reference.contains("..")
            && !reference.chars().any(char::is_whitespace)
    }) || (matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit())))
}
