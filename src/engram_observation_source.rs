// Complete owner identity for ordinary observations. Legacy text is retained
// only for decoding saved requests/checkpoints; new captures never invent it.
enum EngramObservationSourceRefusal {
    Legacy(SourceBasisRefusal),
    OwnerUnavailable,
}

fn engram_observation_identity_valid(
    revision: &str, source_id: Option<&EngramSourceId>,
) -> bool {
    match source_id {
        Some(source_id) => revision.is_empty()
            && serde_json::to_vec(source_id).is_ok_and(|bytes| bytes.len() <= ENGRAM_SOURCE_JSON_LIMIT)
            && source_id.validate(std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET).is_ok(),
        None => !revision.trim().is_empty() && revision.trim() == revision && revision.len() <= 512,
    }
}

fn engram_observation_same_source(
    baseline: &EngramMeasuredBaseline, source: &EngramExecutionSourceBasis,
) -> bool {
    baseline.source_id == source.source_id && baseline.source_revision == source.source_revision
}

impl AppState {
    // Root/store/configuration are pinned before I/O and checked again after
    // it. Both owner probes share one deadline and executable custody check.
    // An unavailable owner supplies no sighting and no diagnostic payload.
    fn engram_observation_source_capture(
        &self, session: &str, place: &EngramBasisPlace, budget: Duration,
    ) -> Result<Option<EngramExecutionSourceBasis>, EngramObservationSourceRefusal> {
        #[cfg(test)]
        {
            let capture = self.inner.lock().expect("state mutex poisoned")
                .test_engram_source_capture.clone();
            if let Some(capture) = capture {
                return capture(place, budget).map_err(EngramObservationSourceRefusal::Legacy);
            }
        }
        let deadline = std::time::Instant::now() + budget.min(ENGRAM_SOURCE_COMMAND_BUDGET);
        let measured = || -> Result<EngramExecutionSourceBasis> {
            engram_source_before_deadline(deadline)?;
            let scope = EngramRuntimeClosingScope::capture(
                &self.inner.lock().expect("state mutex poisoned"), session,
            ).map_err(anyhow::Error::msg)?;
            let root = match place {
                EngramBasisPlace::Workdir(workdir) => engram_worktree_root_path(FsPath::new(workdir)),
                EngramBasisPlace::Named { root, common_dir_key } => {
                    let path = FsPath::new(root);
                    let canonical = fs::canonicalize(path)?;
                    if engram_exact_path_key(&canonical) != engram_exact_path_key(path)
                        || engram_source_root_common_dir(path).as_ref()
                            .is_none_or(|common| engram_exact_path_key(common) != *common_dir_key)
                    {
                        bail!("observation root no longer names its original worktree");
                    }
                    canonical
                }
            };
            let cwd = engram_host_workdir(self.persistence_path.as_ref())?;
            #[cfg(test)]
            let scripted = self.inner.lock().expect("state mutex poisoned")
                .test_observation_source_owner.clone();
            let (_, observation) = engram_source_owner_observe_with(
                &scope.connection.binary_path, &root, &cwd, deadline,
                |binary, cwd, args, until, limit| {
                    #[cfg(test)]
                    if let Some(run) = &scripted { return run(binary, cwd, args, until, limit); }
                    engram_source_owner_run(binary, cwd, args, until, limit)
                },
            )?;
            let source_id = observation.into_source_id(deadline)?;
            scope.require_current(&self.inner.lock().expect("state mutex poisoned"), session)
                .map_err(|_| anyhow::anyhow!("observation authority changed during capture"))?;
            if let EngramBasisPlace::Named { root: named, common_dir_key } = place {
                if fs::canonicalize(named)? != root
                    || engram_source_root_common_dir(FsPath::new(named)).as_ref()
                        .is_none_or(|common| engram_exact_path_key(common) != *common_dir_key)
                {
                    bail!("observation root changed during capture");
                }
            }
            let workspace_id = fs::canonicalize(&root)?.to_string_lossy().into_owned();
            engram_observation_text(&workspace_id, ENGRAM_OBSERVATION_MAX_SOURCE_BYTES, "workspace_id")
                .map_err(anyhow::Error::msg)?;
            engram_source_before_deadline(deadline)?;
            Ok(EngramExecutionSourceBasis {
                workspace_id,
                source_revision: String::new(), source_id: Some(source_id),
                source_root_generation: None, source_root_state: None,
            })
        };
        measured().map(Some).map_err(|_| EngramObservationSourceRefusal::OwnerUnavailable)
    }
}
