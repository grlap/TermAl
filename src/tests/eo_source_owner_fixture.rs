//! Explicit source-owner replies for recorder and observation unit tests.
//! Tests supply source transitions beside their filesystem actions. This fixture
//! never infers source identity from files, Git status or a local hash.
use super::*;

// Independent filesystem controls for tests that also promise not to write
// source. These diagnostics never supply a source_id or an acceptance basis.
pub(crate) fn checkout_state(root: &FsPath) -> (String, String) {
    (
        run_git_test_command_output(root, &["status", "--porcelain", "--untracked-files=all"]),
        run_git_test_command_output(
            root,
            &["diff", "--no-ext-diff", "--no-textconv", "--binary", "HEAD"],
        ),
    )
}

#[derive(Clone, Default)]
pub(crate) struct EOOwnerSources {
    values: Arc<Mutex<BTreeMap<String, EngramSourceId>>>,
    sequence: Arc<std::sync::atomic::AtomicUsize>,
}

impl EOOwnerSources {
    pub(crate) fn get(&self, root: &FsPath) -> EngramSourceId {
        self.values.lock().unwrap().get(&engram_exact_path_key(root)).cloned()
            .unwrap_or_else(|| serde_json::from_value(
                crate::tests::engram_source_command::source_envelope(root, "clean")["source_id"].clone()
            ).unwrap())
    }

    pub(crate) fn set(&self, root: &FsPath, source: EngramSourceId) {
        self.values
            .lock()
            .unwrap()
            .insert(engram_exact_path_key(root), source);
    }

    pub(crate) fn advance(&self, root: &FsPath, path: &str) {
        let ordinal = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let mut source = self.get(root);
        source.changes = vec![EngramSourceIdChange {
            path: path.to_owned(),
            status: " M".into(),
            origin: None,
            blob: Some(format!("{ordinal:040x}")),
            mode: Some(EngramSourceIdMode::Regular),
        }];
        self.set(root, source);
    }

    pub(crate) fn envelope(&self, root: &FsPath) -> Value {
        let source = self.get(root);
        let status = if source.changes.is_empty() {
            "clean"
        } else {
            "not_clean"
        };
        let mut envelope = crate::tests::engram_source_command::source_envelope(root, status);
        envelope["source_id"] = serde_json::to_value(&source).unwrap();
        envelope["entries"] = json!(
            source
                .changes
                .iter()
                .map(|change| {
                    let mut entry = json!({"path": change.path, "status": change.status});
                    if let Some(origin) = &change.origin {
                        entry["origin"] = json!(origin);
                    }
                    entry
                })
                .collect::<Vec<_>>()
        );
        envelope
    }
}
