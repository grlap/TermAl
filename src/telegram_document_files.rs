/*
Telegram document file admission.

Owns: the local-file rules for sending a Markdown file to the linked Telegram
chat. It resolves a requested path against the calling session's workdir,
canonicalizes it (following every symlink), then opens the result once and
checks that same handle's real path, type and size before reading it. The
opened file must remain inside one of the session's allowed roots, be a regular
`.md` file, be non-empty and at most `TELEGRAM_DOCUMENT_MAX_BYTES`, and contain
valid UTF-8 text without NUL bytes.

Deliberately does not own: which roots a session may use, Telegram link state,
the HTTP route, or the upload itself; those live in `telegram_documents.rs` and
`telegram_document_upload.rs`.

Provenance: new module; nothing was moved here from another file.
*/

/// Upper bound for one Markdown document. Telegram bots may upload up to
/// 50 MB; a Markdown note far above 1 MiB is almost certainly not a note.
const TELEGRAM_DOCUMENT_MAX_BYTES: u64 = 1024 * 1024;

/// A Markdown file that passed every admission rule, read into memory once.
#[derive(Debug)]
struct AdmittedTelegramDocument {
    file_name: String,
    canonical_path: PathBuf,
    bytes: Vec<u8>,
}

/// Resolves `requested` and admits it as a Markdown document.
///
/// `base_dir` resolves relative paths and must itself be canonical.
/// `allowed_roots` must be canonical and normalized with
/// `normalize_user_facing_path`; the canonical file must equal or lie under
/// one of them.
fn admit_telegram_markdown_document(
    requested: &str,
    base_dir: &FsPath,
    allowed_roots: &[PathBuf],
) -> Result<AdmittedTelegramDocument, TelegramDocumentError> {
    admit_telegram_markdown_document_before_open(requested, base_dir, allowed_roots, || {})
}

fn admit_telegram_markdown_document_before_open(
    requested: &str,
    base_dir: &FsPath,
    allowed_roots: &[PathBuf],
    before_open: impl FnOnce(),
) -> Result<AdmittedTelegramDocument, TelegramDocumentError> {
    admit_telegram_markdown_document_observed(
        requested,
        base_dir,
        allowed_roots,
        before_open,
        |_| {},
    )
}

// Per-call callbacks expose the two namespace race boundaries to tests. They
// never replace the file handle, its kernel path observation, or the checks.
fn admit_telegram_markdown_document_observed(
    requested: &str,
    base_dir: &FsPath,
    allowed_roots: &[PathBuf],
    before_open: impl FnOnce(),
    after_handle_observation: impl Fn(&FsPath),
) -> Result<AdmittedTelegramDocument, TelegramDocumentError> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(TelegramDocumentError::InvalidRequest(
            "path is required".to_owned(),
        ));
    }
    let raw_path = FsPath::new(requested);
    refuse_telegram_document_namespace(raw_path)?;
    // Reject traversal outright instead of relying on canonicalization alone:
    // a `..` step is never needed to name a file inside the session's roots.
    if raw_path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(TelegramDocumentError::PathOutsideAllowedRoots(
            "path must not contain `..` components".to_owned(),
        ));
    }
    let joined = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        base_dir.join(raw_path)
    };

    let canonical = match fs::canonicalize(&joined) {
        Ok(path) => normalize_user_facing_path(&path),
        Err(err) => return Err(telegram_document_file_error(&joined, err)),
    };

    // Containment is judged on the canonical path, so a symlink inside a root
    // that points outside every root is refused here.
    if !allowed_roots
        .iter()
        .any(|root| canonical == *root || canonical.starts_with(root))
    {
        return Err(TelegramDocumentError::PathOutsideAllowedRoots(format!(
            "`{}` is outside the session's workdir and project root",
            joined.display()
        )));
    }

    // The type is judged on the canonical name, so `notes.md` linking to a
    // non-Markdown file inside the roots is refused too.
    if !canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err(TelegramDocumentError::NotMarkdown(format!(
            "only `.md` files can be sent: {}",
            canonical.display()
        )));
    }
    let file_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            TelegramDocumentError::NotMarkdown(format!(
                "file name is not valid UTF-8: {}",
                canonical.display()
            ))
        })?;

    before_open();
    let mut file = open_telegram_document(&canonical)
        .map_err(|err| telegram_document_file_error(&canonical, err))?;
    let metadata = file
        .metadata()
        .map_err(|err| telegram_document_file_error(&canonical, err))?;
    if !metadata.is_file() || telegram_document_is_reparse(&metadata) {
        return Err(TelegramDocumentError::NotMarkdown(format!(
            "not a regular file: {}",
            canonical.display()
        )));
    }
    validate_telegram_document_handle(&file, &canonical, allowed_roots, &after_handle_observation)?;
    if metadata.len() > TELEGRAM_DOCUMENT_MAX_BYTES {
        return Err(TelegramDocumentError::TooLarge {
            size_bytes: metadata.len(),
        });
    }

    let bytes = read_telegram_document_bytes(&mut file, &canonical)?;
    validate_telegram_document_handle(&file, &canonical, allowed_roots, &after_handle_observation)?;
    if bytes.is_empty() {
        return Err(TelegramDocumentError::EmptyFile(format!(
            "file is empty: {}",
            canonical.display()
        )));
    }
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Err(TelegramDocumentError::NotMarkdown(format!(
            "file is not UTF-8 text: {}",
            canonical.display()
        )));
    }

    Ok(AdmittedTelegramDocument {
        file_name,
        canonical_path: canonical,
        bytes,
    })
}

/// Reads at most one byte past the cap, so a file that grew after the size
/// check is still refused instead of being read without bound.
fn read_telegram_document_bytes(
    file: &mut fs::File,
    path: &FsPath,
) -> Result<Vec<u8>, TelegramDocumentError> {
    use std::io::Read as _;

    let mut bytes = Vec::new();
    file.take(TELEGRAM_DOCUMENT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| {
            TelegramDocumentError::Internal(format!("failed to read {}: {err}", path.display()))
        })?;
    if bytes.len() as u64 > TELEGRAM_DOCUMENT_MAX_BYTES {
        return Err(TelegramDocumentError::TooLarge {
            size_bytes: bytes.len() as u64,
        });
    }
    Ok(bytes)
}
