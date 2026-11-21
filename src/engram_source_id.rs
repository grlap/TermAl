// Owns the producer's complete source_id value and its closed wire admission.
// Split from the source-command envelope adapter in engram_source_command.rs;
// Also presents diagnostic differences without changing source equality.
// Git measurement, executable trust and consumer policy remain their owners.

#[derive(Clone, PartialEq, Eq)]
struct EngramSourceId {
    commit: String,
    tree: String,
    submodules: Vec<EngramSourceIdSubmodule>,
    changes: Vec<EngramSourceIdChange>,
    git_delta: Option<EngramSourceGitDelta>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EngramSourceIdWire {
    GitDelta(EngramSourceIdDeltaWire),
    Legacy(EngramSourceIdLegacyWire),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngramSourceIdDeltaWire {
    commit: String,
    tree: String,
    git_delta: EngramSourceGitDelta,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngramSourceIdLegacyWire {
    commit: String,
    tree: String,
    submodules: Vec<EngramSourceIdSubmodule>,
    changes: Vec<EngramSourceIdChange>,
}

impl<'de> Deserialize<'de> for EngramSourceId {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        Ok(match EngramSourceIdWire::deserialize(decoder)? {
            EngramSourceIdWire::GitDelta(delta) => Self {
                commit: delta.commit,
                tree: delta.tree,
                submodules: Vec::new(),
                changes: Vec::new(),
                git_delta: Some(delta.git_delta),
            },
            EngramSourceIdWire::Legacy(legacy) => Self {
                commit: legacy.commit,
                tree: legacy.tree,
                submodules: legacy.submodules,
                changes: legacy.changes,
                git_delta: None,
            },
        })
    }
}

impl Serialize for EngramSourceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut object =
            serializer.serialize_struct("EngramSourceId", if self.git_delta.is_some() { 3 } else { 4 })?;
        object.serialize_field("commit", &self.commit)?;
        object.serialize_field("tree", &self.tree)?;
        if let Some(delta) = &self.git_delta {
            object.serialize_field("git_delta", delta)?;
        } else {
            object.serialize_field("submodules", &self.submodules)?;
            object.serialize_field("changes", &self.changes)?;
        }
        object.end()
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngramSourceGitDelta {
    sha256: String,
    dirty: bool,
    gitlinks: Vec<EngramSourceGitlink>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngramSourceGitlink {
    path: String,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    head_pin: Option<String>,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    index_pin: Option<String>,
    #[serde(deserialize_with = "engram_source_required_nullable")]
    checkout_head: Option<String>,
}

// Diagnostics may reach agent notices. Keep identity data on the producer
// wire and in durable records, never in automatic Debug output.
impl std::fmt::Debug for EngramSourceId {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut summary = out.debug_struct("EngramSourceIdSummary");
        summary.field("commit", &self.commit).field("tree", &self.tree);
        if let Some(delta) = &self.git_delta {
            summary.field("git_delta_dirty", &delta.dirty)
                .field("gitlinks", &delta.gitlinks.len());
        } else {
            summary.field("submodules", &self.submodules.len())
                .field("changes", &self.changes.len());
        }
        summary.finish()
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
    if before.compare(after).is_none() {
        return out;
    }
    for (kind, old, new) in [("root_commit", &before.commit, &after.commit), ("root_tree", &before.tree, &after.tree)] {
        if old != new { out.push(EngramSourceIdDifference { path:".".into(),kind:kind.into(),before:json!(old),after:json!(new) }); }
    }
    if let (Some(old), Some(new)) = (&before.git_delta, &after.git_delta) {
        if old != new {
            out.push(EngramSourceIdDifference {
                path: ".".into(),
                kind: "git_delta".into(),
                before: json!({"dirty": old.dirty, "gitlinks": old.gitlinks.len()}),
                after: json!({"dirty": new.dirty, "gitlinks": new.gitlinks.len()}),
            });
        }
        return out;
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
            "git_delta" => "changed Git delta",
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
    fn git_delta_description(&self) -> Option<String> {
        self.git_delta.as_ref().map(|delta| {
            let state = if delta.dirty { "dirty" } else { "clean" };
            format!("Git delta {state}, {} selected gitlinks", delta.gitlinks.len())
        })
    }

    fn same_format(&self, other: &Self) -> bool {
        self.git_delta.is_some() == other.git_delta.is_some()
    }

    fn compare(&self, other: &Self) -> Option<bool> {
        self.same_format(other).then(|| self == other)
    }

    fn is_clean(&self) -> bool {
        match &self.git_delta {
            Some(delta) => !delta.dirty,
            None => self.changes.is_empty() && self.submodules.iter().all(|child| child.changes.is_empty()),
        }
    }

    fn validate(&self, deadline: std::time::Instant) -> Result<()> {
        engram_source_before_deadline(deadline)?;
        if !engram_source_id_oid(&self.commit) || !engram_source_id_oid(&self.tree) {
            bail!("Engram source_id returned an invalid root commit or tree");
        }
        if let Some(delta) = &self.git_delta {
            if !self.changes.is_empty() || !self.submodules.is_empty()
                || delta.sha256.len() != 64 || !is_lowercase_hex(&delta.sha256)
                || (!delta.dirty && !delta.gitlinks.is_empty())
            {
                bail!("Engram source_id returned an invalid Git delta");
            }
            let mut previous: Option<&str> = None;
            for link in &delta.gitlinks {
                engram_source_before_deadline(deadline)?;
                if !engram_source_id_path(&link.path) || link.path.contains('\r')
                    || (link.head_pin.is_none() && link.index_pin.is_none())
                    || (link.index_pin.is_some() && link.checkout_head.is_none())
                    || [&link.head_pin, &link.index_pin, &link.checkout_head].into_iter()
                        .flatten().any(|commit| !engram_source_id_oid(commit))
                    || previous.is_some_and(|path| path >= link.path.as_str())
                {
                    bail!("Engram source_id returned an invalid or unordered gitlink");
                }
                previous = Some(&link.path);
            }
            return engram_source_before_deadline(deadline);
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
