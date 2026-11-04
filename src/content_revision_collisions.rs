// Owns the one content-v1 collision refusal: distinct tracked spellings that
// differ only by case or canonical Unicode normalization naming one file.
// Does not normalize hash input, judge Git cleanliness, or reject distinct-name
// hard links and terminal symlinks. Reuses the checked reader's opened handle
// and the existing platform file-identity primitive.

type ContentRevisionFileKey = (u8, u64, u64);

#[derive(Default)]
struct ContentRevisionCollisions {
    candidates: std::collections::HashMap<String, String>,
    seen: std::collections::HashMap<(String, ContentRevisionFileKey), String>,
    observed: Vec<(String, ContentRevisionFileKey)>,
}

fn content_revision_spelling_key(path: &str) -> String {
    let nfc = icu_normalizer::ComposingNormalizerBorrowed::new_nfc();
    // Decompose BEFORE folding: j + caron has an NFC composite only in
    // lowercase, but its two case spellings must still share a key.
    let normalized = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(path);
    // Pick the least scalar in each SIMPLE case-fold equivalence class.
    // Full folding would expand sharp-s to ss and reject distinct-name hard
    // links. Reuse the locked Unicode tables rather than approximating with
    // lowercase (which also misses simple folds such as long-s-t ligatures).
    let folded: String = normalized.chars().map(|character| {
        if character.is_ascii() {
            return character.to_ascii_uppercase();
        }
        use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};
        let mut class = ClassUnicode::new([ClassUnicodeRange::new(character, character)]);
        class.case_fold_simple();
        class.iter().flat_map(|range| range.start()..=range.end())
            // This class includes combining ypogegrammeni and ordinary iota.
            // Use its letter representative so final NFC cannot reorder a
            // formerly spacing letter with adjacent combining marks.
            .filter(|candidate| *candidate != '\u{0345}')
            .min().unwrap_or(character)
    }).collect();
    nfc.normalize(&folded).into_owned()
}

fn content_revision_file_key(
    file: &fs::File,
    metadata: &fs::Metadata,
) -> Result<ContentRevisionFileKey> {
    Ok(match federation_image_file_identity(file, metadata)? {
        FederationImageFileIdentity::Windows { index: 0, .. } => {
            bail!("tracked spelling identity is unavailable: Windows returned a zero file index")
        }
        FederationImageFileIdentity::Windows { volume, index } => (0, volume, index),
        FederationImageFileIdentity::Unix { dev, inode } => (1, dev, inode),
    })
}

impl ContentRevisionCollisions {
    fn from_index(index: &[u8], deadline: std::time::Instant) -> Result<Self> {
        let mut result = Self::default();
        let mut first = std::collections::HashMap::<String, String>::new();
        for entry in index.split(|byte| *byte == 0).filter(|entry| !entry.is_empty()) {
            check_capture_cancellation()?;
            if std::time::Instant::now() >= deadline {
                bail!("content revision deadline exceeded");
            }
            let tab = entry.iter().position(|byte| *byte == b'\t')
                .context("malformed index listing")?;
            let path = std::str::from_utf8(&entry[tab + 1..])
                .context("non-UTF8 path in a content revision")?;
            let key = content_revision_spelling_key(path);
            if let Some(previous) = first.get(&key) {
                // Conflict stages repeat an exact path, which is not a new
                // spelling and keeps its existing content-v1 treatment.
                if previous != path {
                    result.candidates.insert(previous.clone(), key.clone());
                    result.candidates.insert(path.to_owned(), key);
                }
            } else {
                first.insert(key, path.to_owned());
            }
        }
        Ok(result)
    }

    fn observe(&mut self, path: &str, file: &fs::File, metadata: &fs::Metadata) -> Result<()> {
        let Some(spelling) = self.candidates.get(path) else {
            return Ok(());
        };
        let identity = content_revision_file_key(file, metadata)?;
        if let Some(previous) = self.seen.insert((spelling.clone(), identity), path.to_owned()) {
            bail!("distinct tracked paths {previous:?} and {path:?} resolve to the same file");
        }
        self.observed.push((path.to_owned(), identity));
        Ok(())
    }

    fn recheck(&self, root: &FsPath, deadline: std::time::Instant) -> Result<()> {
        let mut parents = std::collections::HashSet::new();
        for (path, identity) in &self.observed {
            check_capture_cancellation()?;
            if std::time::Instant::now() >= deadline {
                bail!("content revision deadline exceeded");
            }
            let full = content_revision_path(root, path, &mut parents)?
                .context("tracked collision candidate disappeared during capture")?;
            let before = fs::symlink_metadata(&full)?;
            if !before.is_file() || before.file_type().is_symlink() {
                bail!("tracked collision candidate {path:?} changed during capture");
            }
            let file = review_freeze_open_without_waiting(&full, &before)?;
            let opened = file.metadata()?;
            if review_freeze_reparse_appeared(&before, &opened)
                || !review_freeze_same_entry(&before, &opened)
                || content_revision_file_key(&file, &opened)? != *identity
            {
                bail!("tracked collision candidate {path:?} changed during capture");
            }
        }
        Ok(())
    }
}
