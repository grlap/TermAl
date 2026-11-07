// Scripted owner boundary for native protocol tests. Objects are explicit
// fixture inputs, not an independent Git-state reader or producer conformance
// evidence. The real M1 parser, pin checks and declaration membership still run.
use super::*;
use super::engram_source_owner::{source_owner_fixture_binary, source_owner_output, source_owner_reply};

#[derive(Clone)]
pub(super) struct NativeSourceOwner {
    root: PathBuf,
    pub(super) pin: EngramSourceOwnerPin,
    identity: Arc<Mutex<EngramSourceId>>,
}

impl NativeSourceOwner {
    pub(super) fn new(root: &FsPath) -> Self {
        let directory = root.join(".tmp/native-source-owner");
        fs::create_dir_all(&directory).unwrap();
        let binary = source_owner_fixture_binary(&directory);
        let pin = engram_source_owner_metadata_with(&binary, root, root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
            |_, _, args, _, _| Ok(source_owner_reply(args))).unwrap();
        let identity = EngramSourceId {
            commit: run_git_test_command_output(root, &["rev-parse", "HEAD"]),
            tree: run_git_test_command_output(root, &["show", "-s", "--format=%T", "HEAD"]),
            submodules: Vec::new(), changes: Vec::new(),
        };
        Self { root: fs::canonicalize(root).unwrap(), pin, identity: Arc::new(Mutex::new(identity)) }
    }

    pub(super) fn identity(&self) -> EngramSourceId { self.identity.lock().unwrap().clone() }
    pub(super) fn replace(&self, identity: EngramSourceId) { *self.identity.lock().unwrap() = identity; }

    pub(super) fn refresh_commit(&self) {
        let mut identity = self.identity.lock().unwrap();
        identity.commit = run_git_test_command_output(&self.root, &["rev-parse", "HEAD"]);
        identity.tree = run_git_test_command_output(&self.root, &["show", "-s", "--format=%T", "HEAD"]);
    }

    pub(super) fn record_file_change(&self, path: &str, status: &str) {
        let mut identity = self.identity();
        identity.changes = vec![EngramSourceIdChange {
            path:path.into(), status:status.into(), origin:None,
            blob:Some(run_git_test_command_output(&self.root, &["hash-object", "--", path])),
            mode:Some(EngramSourceIdMode::Regular),
        }];
        self.replace(identity);
    }

    fn capture(&self, pin: &EngramSourceOwnerPin, root: &FsPath, deadline: std::time::Instant)
        -> Result<EngramSourceId> {
        assert_eq!(fs::canonicalize(root).unwrap(), self.root);
        let source_id = self.identity();
        let mut entries: Vec<Value> = source_id.changes.iter().map(|change|
            json!({"path":change.path,"status":change.status,"origin":change.origin})).collect();
        entries.extend(source_id.submodules.iter().filter(|child| !child.changes.is_empty())
            .map(|child| json!({"path":child.path,"status":" M"})));
        let envelope = json!({"version":1,"status":if entries.is_empty() {"clean"} else {"not_clean"},
            "root":engram_exact_path_key(root),"source":format!("git-commit:{}", source_id.commit),
            "tree":source_id.tree,"entries":entries,"refusal":null,"source_id":source_id});
        engram_source_id_with(pin, root, deadline, |_, _, _, _, _|
            Ok(source_owner_output(0, serde_json::to_vec(&envelope).unwrap())))
    }

    pub(super) fn source(&self, binding: &EngramDeclaredBinding, deadline: std::time::Instant)
        -> Result<DeclaredRunSource> {
        declared_run_source_with_owner(binding, &self.pin, deadline, None,
            |pin, root, until| self.capture(pin, root, until))
    }

    pub(super) fn snapshot(&self, deadline: std::time::Instant, timings: Option<Arc<SourceReadTimings>>)
        -> Result<DeclaredRunSnapshot> {
        declared_run_snapshot_with_owner(&self.root, &self.pin, deadline, timings,
            |pin, root, until| self.capture(pin, root, until))
    }

    pub(super) fn install(&self, state: &AppState, project_id: &str) {
        let mut inner = state.inner.lock().unwrap();
        inner.projects.iter_mut().find(|project| project.id == project_id).unwrap()
            .engram.as_mut().unwrap().binary_path = Some(self.pin.binary_path.clone());
        let source = self.clone();
        inner.test_declared_run_source = Some(Arc::new(move |binding, pin, deadline| {
            assert_eq!(pin, &source.pin);
            source.source(binding, deadline)
        }));
        inner.test_native_source_owner_metadata = Some(Arc::new(|state, session, deadline|
            state.source_owner_metadata_snapshot_with(session, deadline,
                |_, _, args, _, _| Ok(source_owner_reply(args)))));
    }
}
