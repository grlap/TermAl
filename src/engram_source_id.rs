// Owns the producer's complete source_id value and its closed wire admission.
// Split from the source-command envelope adapter in engram_source_command.rs;
// Git measurement, executable trust and consumer policy remain their owners.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngramSourceId {
    commit: String,
    tree: String,
    submodules: Vec<EngramSourceIdSubmodule>,
    changes: Vec<EngramSourceIdChange>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngramSourceIdSubmodule {
    path: String,
    commit: String,
    changes: Vec<EngramSourceIdChange>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngramSourceIdChange {
    path: String,
    status: String,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    origin: Option<String>,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    blob: Option<String>,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    mode: Option<EngramSourceIdMode>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
enum EngramSourceIdMode {
    #[serde(rename = "100644")]
    Regular,
    #[serde(rename = "100755")]
    Executable,
    #[serde(rename = "120000")]
    Symlink,
}

fn engram_source_id_oid(id: &str) -> bool {
    id.len() == 40 && is_lowercase_hex(id)
}

fn engram_source_id_path(path: &str) -> bool {
    !path.is_empty() && !path.contains(['\0', '\n'])
        && FsPath::new(path).components().all(|part|
            matches!(part, std::path::Component::Normal(_)))
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn engram_source_id_changes(
    changes: &[EngramSourceIdChange], deadline: std::time::Instant,
) -> Result<()> {
    let mut previous: Option<(&[u8], &[u8], Option<&[u8]>)> = None;
    for change in changes {
        engram_source_before_deadline(deadline)?;
        let renamed = change.status.bytes().any(|byte| matches!(byte, b'R' | b'C'));
        if !engram_source_id_path(&change.path)
            || change.status.len() != 2 || change.status == "  " || change.status == "!!"
            || !change.status.bytes().all(|byte| b" MADRCUT?".contains(&byte))
            || renamed != change.origin.is_some()
            || change.origin.as_deref().is_some_and(|origin| !engram_source_id_path(origin))
        {
            bail!("Engram source_id returned an invalid change");
        }
        match (&change.blob, change.mode) {
            (Some(blob), Some(_)) if engram_source_id_oid(blob) => {}
            (None, None) if change.status.contains('D') => {}
            _ => bail!("Engram source_id returned an incoherent change blob or mode"),
        }
        // Do not sort or deduplicate producer identity. A staged deletion and
        // a recreated untracked file intentionally retain two status records.
        let key = (change.path.as_bytes(), change.status.as_bytes(),
            change.origin.as_deref().map(str::as_bytes));
        if previous.is_some_and(|previous| previous > key) {
            bail!("Engram source_id changes are not in UTF-8 byte order");
        }
        previous = Some(key);
    }
    Ok(())
}

impl EngramSourceId {
    fn validate(&self, deadline: std::time::Instant) -> Result<()> {
        engram_source_before_deadline(deadline)?;
        if !engram_source_id_oid(&self.commit) || !engram_source_id_oid(&self.tree) {
            bail!("Engram source_id returned an invalid root commit or tree");
        }
        engram_source_id_changes(&self.changes, deadline)?;
        let mut previous: Option<&[u8]> = None;
        for child in &self.submodules {
            engram_source_before_deadline(deadline)?;
            if !engram_source_id_path(&child.path) || !engram_source_id_oid(&child.commit)
                || previous.is_some_and(|previous| previous >= child.path.as_bytes())
            {
                bail!("Engram source_id returned an invalid or unordered submodule");
            }
            engram_source_id_changes(&child.changes, deadline)?;
            previous = Some(child.path.as_bytes());
        }
        engram_source_before_deadline(deadline)
    }
}
