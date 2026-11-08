// Owns the producer's complete source_id value and its closed wire admission.
// Split from the source-command envelope adapter in engram_source_command.rs;
// Also presents diagnostic differences without changing source equality.
// Git measurement, executable trust and consumer policy remain their owners.

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngramSourceId {
    commit: String,
    tree: String,
    submodules: Vec<EngramSourceIdSubmodule>,
    changes: Vec<EngramSourceIdChange>,
}

// Diagnostics may reach agent notices. Keep identity data on the producer
// wire and in durable records, never in automatic Debug output.
impl std::fmt::Debug for EngramSourceId {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("EngramSourceIdSummary")
            .field("commit", &self.commit).field("tree", &self.tree)
            .field("submodules", &self.submodules.len())
            .field("changes", &self.changes.len()).finish()
    }
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

// Presentation of differences between two complete owner observations. This
// never participates in identity equality or source admission.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct EngramSourceIdDifference {
    path: String,
    kind: String,
    before: Value,
    after: Value,
}

fn engram_source_id_differences(before: &EngramSourceId, after: &EngramSourceId) -> Vec<EngramSourceIdDifference> {
    fn files(prefix: &str, before: &[EngramSourceIdChange], after: &[EngramSourceIdChange], out: &mut Vec<EngramSourceIdDifference>) {
        let group = |entries: &[EngramSourceIdChange]| {
            let mut grouped = BTreeMap::<String, Vec<EngramSourceIdChange>>::new();
            for entry in entries { grouped.entry(entry.path.clone()).or_default().push(entry.clone()); }
            grouped
        };
        let old_entries = group(before);
        let new_entries = group(after);
        let paths: std::collections::BTreeSet<_> = old_entries.keys().chain(new_entries.keys()).collect();
        for path in paths {
            let old = old_entries.get(path).map_or(&[][..], Vec::as_slice);
            let new = new_entries.get(path).map_or(&[][..], Vec::as_slice);
            if old == new { continue; }
            out.push(EngramSourceIdDifference {
                path: format!("{prefix}{path}"),
                kind: if old.iter().chain(new).any(|entry| entry.status == "??") { "untracked_file" } else { "tracked_file" }.into(),
                before: json!(old), after: json!(new),
            });
        }
    }
    let mut out = Vec::new();
    for (kind, old, new) in [("root_commit", &before.commit, &after.commit), ("root_tree", &before.tree, &after.tree)] {
        if old != new { out.push(EngramSourceIdDifference { path:".".into(),kind:kind.into(),before:json!(old),after:json!(new) }); }
    }
    files("", &before.changes, &after.changes, &mut out);
    let old_children: BTreeMap<_, _> = before.submodules.iter().map(|child| (&child.path, child)).collect();
    let new_children: BTreeMap<_, _> = after.submodules.iter().map(|child| (&child.path, child)).collect();
    let paths: std::collections::BTreeSet<_> = old_children.keys().chain(new_children.keys()).copied().collect();
    for path in paths {
        let old = old_children.get(path).copied();
        let new = new_children.get(path).copied();
        if old.map(|child| &child.commit) != new.map(|child| &child.commit) {
            out.push(EngramSourceIdDifference { path:path.clone(),kind:"submodule_head".into(),
                before:json!(old.map(|child| &child.commit)),after:json!(new.map(|child| &child.commit)) });
        }
        files(&format!("{path}/"), old.map_or(&[], |child| child.changes.as_slice()),
            new.map_or(&[], |child| child.changes.as_slice()), &mut out);
    }
    out
}

fn engram_source_id_change_detail(changes: &[EngramSourceIdDifference]) -> String {
    let entries = changes.iter().map(|change| {
        let kind = match change.kind.as_str() {
            "untracked_file" => "changed non-ignored untracked file",
            "tracked_file" => "edited tracked file",
            "submodule_head" => "changed submodule HEAD",
            "root_commit" => "changed root commit",
            _ => "changed root tree",
        };
        format!("{kind} {:?}", change.path)
    }).collect::<Vec<_>>().join("; ");
    format!("source changed during the run: {entries}")
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
