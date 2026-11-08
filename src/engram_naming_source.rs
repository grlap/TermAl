// Private source capture for naming and sealing. Naming retains its existing
// Base-service authority; ordinary observation admission is not a prerequisite.
// Acceptance's historical revision capture remains a separate consumer.

fn engram_naming_root_current(root: &str, common_dir_key: &str) -> bool {
    let path = FsPath::new(root);
    fs::canonicalize(path).is_ok_and(|canonical|
        engram_exact_path_key(&canonical) == engram_exact_path_key(path))
        && engram_source_root_common_dir(path).is_some_and(|common|
            engram_exact_path_key(&common) == common_dir_key)
}

impl AppState {
    fn engram_naming_source_basis_within(
        &self, target: &EngramBindingTarget, root: &str, common_dir_key: &str,
        budget: Duration,
    ) -> Option<EngramExecutionSourceBasis> {
        let budget = budget.min(ENGRAM_SOURCE_COMMAND_BUDGET);
        if budget.is_zero() { return None; }
        let deadline = std::time::Instant::now() + budget;
        let live = self.inner.lock().expect("state mutex poisoned")
            .engram_turn_basis_captures_live.clone();
        let state = self.clone();
        let target = target.clone();
        let root = root.to_owned();
        let common_dir_key = common_dir_key.to_owned();
        bounded_content_revision_capture(
            &live, &self.capture_lifetime, ENGRAM_TURN_BASIS_CAPTURE_LIMIT, budget,
            move || state.engram_naming_source_basis_until(&target, &root, &common_dir_key, deadline)
                .ok(),
        )
    }

    fn engram_naming_source_basis_until(
        &self, target: &EngramBindingTarget, root: &str, common_dir_key: &str,
        deadline: std::time::Instant,
    ) -> Result<EngramExecutionSourceBasis> {
        let current = |inner: &StateInner| {
            Self::engram_source_root_target_locked(inner, &target.connection.session_id)
                .ok().flatten().is_some_and(|now|
                    now.connection == target.connection
                        && now.project_id == target.project_id
                        && now.settings.same_work_service_settings(&target.settings)
                        && now.external_ref == target.external_ref
                        && now.runtime_snapshot == target.runtime_snapshot)
        };
        engram_source_before_deadline(deadline)?;
        if !current(&self.inner.lock().expect("state mutex poisoned"))
            || !engram_naming_root_current(root, common_dir_key)
        {
            bail!("naming source scope is unavailable");
        }
        let cwd = engram_host_workdir(self.persistence_path.as_ref())?;
        #[cfg(test)]
        let scripted = self.inner.lock().expect("state mutex poisoned")
            .test_observation_source_owner.clone();
        let (_, observation) = engram_source_owner_observe_with(
            &target.connection.binary_path, FsPath::new(root), &cwd, deadline,
            |binary, cwd, args, until, limit| {
                #[cfg(test)]
                if let Some(run) = &scripted { return run(binary, cwd, args, until, limit); }
                engram_source_owner_run(binary, cwd, args, until, limit)
            },
        )?;
        let source_id = observation.into_source_id(deadline)?;
        if !current(&self.inner.lock().expect("state mutex poisoned"))
            || !engram_naming_root_current(root, common_dir_key)
        {
            bail!("naming source scope changed during capture");
        }
        engram_source_before_deadline(deadline)?;
        Ok(EngramExecutionSourceBasis {
            workspace_id: root.to_owned(), source_revision: String::new(),
            source_id: Some(source_id), source_root_generation: None, source_root_state: None,
        })
    }
}
