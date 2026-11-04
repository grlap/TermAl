// Bounded source measurement shared by ordinary observation, naming and
// explicit verification. Filesystem capture runs off the state lock and
// cannot authorize delivery or create a control obligation.

const ENGRAM_SOURCE_BASIS_MAX_BYTES: usize = 512;

type EngramSourceCapture = std::result::Result<Option<EngramExecutionSourceBasis>, SourceBasisRefusal>;

#[cfg(test)]
fn engram_turn_end_basis(
    place: &EngramBasisPlace,
    sealed: Option<&(String, String)>,
    captured: Option<EngramExecutionSourceBasis>,
) -> Option<EngramExecutionSourceBasis> {
    if captured.is_some() {
        return captured;
    }
    let EngramBasisPlace::Named { root, .. } = place else {
        return None;
    };
    let (sealed_root, revision) = sealed.filter(|(sealed_root, _)| sealed_root == root)?;
    engram_named_root_absent(root).then(|| EngramExecutionSourceBasis {
        source_root_generation: None,
        source_root_state: None,
        workspace_id: sealed_root.clone(),
        source_revision: revision.clone(),
    })
}

#[cfg(test)]
fn engram_execution_source_basis(workdir: &FsPath) -> Option<EngramExecutionSourceBasis> {
    engram_execution_source_basis_with(workdir, ORDINARY_COMMIT_INPUT_DEFAULT)
}

/// [`engram_execution_source_basis`] with the committed-input switch given
/// ([`source_revision`]). Off, the basis is the content revision. On, a clean
/// worktree's is its HEAD commit, and one that is not clean has none, logged
/// with the paths Git reported, exactly as a revision that cannot be taken.
#[cfg(test)]
fn engram_execution_source_basis_with(
    workdir: &FsPath,
    ordinary_commit_input: bool,
) -> Option<EngramExecutionSourceBasis> {
    engram_execution_source_capture_with(workdir, ordinary_commit_input).ok().flatten()
}

fn engram_execution_source_capture_with(
    workdir: &FsPath,
    ordinary_commit_input: bool,
) -> EngramSourceCapture {
    // A session may work in a subdirectory: its source identity is its
    // worktree's, which the fingerprint takes from the worktree root.
    let root = engram_worktree_root_path(workdir);
    let resolved_workdir = std::fs::canonicalize(workdir).ok();
    match source_revision(&root, ordinary_commit_input) {
        Ok(SourceRevision::Taken { root, revision }) => {
            Ok(engram_bounded_source_basis(workdir, &root, revision))
        }
        Ok(SourceRevision::NotClean { dirty_paths }) => {
            eprintln!(
                "engram> no source basis for workspace {}: {}",
                workdir.display(),
                source_revision_not_clean_reason(&dirty_paths)
            );
            Ok(None)
        }
        Err(error) => {
            eprintln!(
                "engram> no source basis for workspace {}: {error:#}",
                workdir.display()
            );
            match error.downcast_ref::<SourceBasisRefusal>() {
                // An absent workdir can make the ancestor walk select an
                // enclosing repository. Its refusal does not establish a
                // permanent cause for this session's workspace. Keep a path
                // that disappeared or changed roots during capture transient.
                Some(refusal) if resolved_workdir.as_ref().is_some_and(|resolved| {
                    resolved.starts_with(&root)
                        && std::fs::canonicalize(workdir).ok().as_ref() == Some(resolved)
                        && engram_worktree_root_path(resolved) == root
                }) => Err(*refusal),
                _ => Ok(None),
            }
        }
    }
}

/// The basis of a workspace frozen at worktree `root`. Engram refuses a basis
/// field over 512 bytes, and with it the whole report, so a root that long
/// leaves the report without a basis, and logged, rather than refused.
fn engram_bounded_source_basis(
    workdir: &FsPath,
    root: &FsPath,
    source_revision: String,
) -> Option<EngramExecutionSourceBasis> {
    let workspace_id = root.to_string_lossy().into_owned();
    if workspace_id.len() > ENGRAM_SOURCE_BASIS_MAX_BYTES {
        eprintln!(
            "engram> no source basis for workspace {}: its root exceeds {} bytes",
            workdir.display(),
            ENGRAM_SOURCE_BASIS_MAX_BYTES
        );
        return None;
    }
    Some(EngramExecutionSourceBasis {
        source_root_generation: None,
        source_root_state: None,
        workspace_id,
        source_revision,
    })
}

/// At most this many threads of one host take turn source bases at once,
/// healthy and abandoned alike. A capture past it counts as not taken.
const ENGRAM_TURN_BASIS_CAPTURE_LIMIT: usize = 32;

impl AppState {
    /// The source basis at `place` ([`engram_place_source_basis`]) on its own
    /// thread, waited for at most `budget`. The capture reads every listed
    /// file and a read cannot be interrupted, so a read that stalls must not
    /// hold the turn that waits for it: past the budget the basis counts as
    /// not taken, the conservative path, and the thread is left to finish
    /// ([`bounded_content_revision_capture`]).
    fn engram_source_basis_within(
        &self,
        place: &EngramBasisPlace,
        budget: Duration,
    ) -> Option<EngramExecutionSourceBasis> {
        self.engram_source_capture_within(place, budget).ok().flatten()
    }

    /// Preserve only typed definitive refusals. Exhausted capacity, deadline,
    /// panic or any other missing capture remains transient absence.
    fn engram_source_capture_within(
        &self,
        place: &EngramBasisPlace,
        budget: Duration,
    ) -> EngramSourceCapture {
        #[cfg(test)]
        {
            let capture = self.inner.lock().expect("state mutex poisoned")
                .test_engram_source_capture.clone();
            if let Some(capture) = capture {
                return capture(place, budget);
            }
        }
        let live = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_turn_basis_captures_live
            .clone();
        let place = place.clone();
        bounded_content_revision_capture(
            &live,
            &self.capture_lifetime,
            ENGRAM_TURN_BASIS_CAPTURE_LIMIT,
            budget,
            move || Some(engram_place_source_capture(&place)),
        )
        .unwrap_or(Ok(None))
    }

    /// The closing basis of a turn at `place` (`engram_turn_end_basis`), on
    /// its own thread and waited for at most `budget`: the capture and, when
    /// it fails, the check that a sealed root is gone both touch the file
    /// system, so both stay inside the bound. Past the budget, or at the
    /// capture limit, there is no basis.
    #[cfg(test)]
    fn engram_turn_end_basis_within(
        &self,
        place: &EngramBasisPlace,
        sealed: Option<&(String, String)>,
        budget: Duration,
    ) -> Option<EngramExecutionSourceBasis> {
        let live = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_turn_basis_captures_live
            .clone();
        let place = place.clone();
        let sealed = sealed.cloned();
        bounded_content_revision_capture(
            &live,
            &self.capture_lifetime,
            ENGRAM_TURN_BASIS_CAPTURE_LIMIT,
            budget,
            move || {
                engram_turn_end_basis(&place, sealed.as_ref(), engram_place_source_basis(&place))
            },
        )
    }

}
