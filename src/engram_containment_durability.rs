// Owns acknowledged settings/obligation persistence on the existing writer.
// Does not send authority requests, interrupt providers or settle ownership.

#[cfg(test)]
thread_local! {
    static TEST_CONTAINMENT_BEFORE_ACK_RETURN: std::cell::RefCell<Option<Box<dyn FnOnce()>>>
        = const { std::cell::RefCell::new(None) };
}

#[derive(Clone)]
struct EngramProjectContainmentFence {
    project_id: String,
    settings: EngramProjectSettings,
    obligations: Value,
    sessions: Vec<(String, Value)>,
    absent_sessions: Vec<String>,
    checkpoint_effects: Option<EngramCheckpointEffectsFence>,
}

#[derive(Clone, PartialEq, Eq)]
struct EngramCheckpointEffectsFence {
    observations: Vec<TestRunCommandObservation>,
    source_sightings: Value,
    session: Option<(String, Value)>,
}

fn checkpoint_effect_session_image(row: &Value) -> Value {
    json!({
        "owedBinds": row.get("engramOwedBinds"),
        "pendingBatches": row.get("engramPendingBatches"),
    })
}

fn engram_containment_session_image(row: &Value) -> Value {
    json!({
        "routingToken": row.get("engramRoutingToken"),
        "activeGrant": row.get("engramOpenGrantId"),
        "uncertainGrant": row.get("engramUncertainGrantId"),
        "beginsRecorded": row.get("engramBeginsRecorded"),
        "generation": row.get("engramDispatchGeneration"),
        "checkpointOperation": row.get("engramCheckpointOperation"),
        // A transfer must acknowledge the exact prepared admission successor
        // as well as its metadata ledger, on the same writer connection.
        "queue": row.get("queuedPrompts"),
        "admissionRetry": row.get("engramAdmissionRetry"),
    })
}

fn engram_project_obligations_image(
    ledger: &EngramSessionObligationLedger,
    project_id: &str,
) -> Value {
    json!({
        "entries": ledger
            .entries
            .iter()
            .filter(|entry| entry.project_id == project_id)
            .collect::<Vec<_>>(),
        "checkpointOperations": ledger.checkpoint_operations.iter()
            .filter(|operation| operation.protection.project_id == project_id).collect::<Vec<_>>(),
    })
}

impl EngramCheckpointEffectsFence {
    // Receipt effects use the same writer/readback proof for ordinary and
    // recovered checkpoints. Historical evidence is addressed by the saved
    // store, binding and producer identity; row effects need a current owner.
    fn capture(
        inner: &StateInner,
        operation: &EngramCheckpointOperation,
        owns_session: bool,
    ) -> Result<Self> {
        let EngramControlRequest::TurnCheckpoint { report, .. } = &operation.original.request
            else { bail!("Not a checkpoint") };
        let observations = inner.test_run_observations.values().filter(|row| {
            row.evidence.iter().any(|association| {
                operation.original.settings.authority_store_key.as_ref() == Some(&association.store)
                    && operation.original.work_binding.as_ref() == Some(&association.binding)
                    && report.observations.iter().any(|producer|
                        producer.observation_id == association.producer_observation_id)
            })
        }).cloned().collect();
        let session = if owns_session {
            inner.find_session_index(&operation.protection.local_session_id)
                .map(|index| -> Result<_> {
                    let row = serde_json::to_value(PersistedSessionRecord::from_record(&inner.sessions[index]))?;
                    Ok((operation.protection.local_session_id.clone(), checkpoint_effect_session_image(&row)))
                }).transpose()?
        } else { None };
        Ok(Self {
            observations, source_sightings: serde_json::to_value(&inner.engram_source_sightings)?, session,
        })
    }
}

impl EngramProjectContainmentFence {
    fn capture_checkpoint_effects(
        &mut self,
        inner: &StateInner,
        operation: &EngramCheckpointOperation,
        owns_session: bool,
    ) -> Result<()> {
        self.checkpoint_effects = Some(EngramCheckpointEffectsFence::capture(inner, operation, owns_session)?);
        Ok(())
    }

    fn capture(inner: &StateInner, project_id: &str, session_ids: &[String]) -> Result<Self> {
        inner.engram_session_obligations.validate()?;
        let settings = inner
            .find_project(project_id)
            .and_then(|project| project.engram.clone())
            .ok_or_else(|| anyhow!("containment project settings disappeared"))?;
        let sessions = session_ids
            .iter()
            .map(|id| {
                let index = inner
                    .find_session_index(id)
                    .ok_or_else(|| anyhow!("containment session disappeared"))?;
                let row = serde_json::to_value(PersistedSessionRecord::from_record(
                    &inner.sessions[index],
                ))?;
                Ok((id.clone(), engram_containment_session_image(&row)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            project_id: project_id.to_owned(),
            settings,
            obligations: engram_project_obligations_image(
                &inner.engram_session_obligations,
                project_id,
            ),
            sessions,
            absent_sessions: Vec::new(),
            checkpoint_effects: None,
        })
    }

    fn is_already_durable(&self, connection: &rusqlite::Connection) -> Result<bool> {
        let fields: &[&str] = if self.checkpoint_effects.is_some() {
            &["projects", "engramSessionObligations", "testRunObservations", "engramSourceSightings"]
        } else {
            &["projects", "engramSessionObligations"]
        };
        let Some(metadata) = read_metadata_for_fence(connection, fields)?
        else {
            return Ok(false);
        };
        // Match the persisted settings shape. Compatibility-only fields
        // marked serde(skip) cannot be part of a durable content proof.
        let stored_settings = metadata
            .projects
            .iter()
            .find(|project| project.id == self.project_id)
            .and_then(|project| project.engram.as_ref())
            .map(serde_json::to_value)
            .transpose()?;
        if stored_settings != Some(serde_json::to_value(&self.settings)?)
            || engram_project_obligations_image(
                &metadata.engram_session_obligations,
                &self.project_id,
            ) != self.obligations
        {
            return Ok(false);
        }
        for (id, expected) in &self.sessions {
            let stored: Option<String> = connection
                .query_row(
                    "SELECT value_json FROM sessions WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(stored) = stored else {
                return Ok(false);
            };
            if engram_containment_session_image(&serde_json::from_str::<Value>(&stored)?)
                != *expected
            {
                return Ok(false);
            }
        }
        for id in &self.absent_sessions {
            if connection.query_row("SELECT 1 FROM sessions WHERE id=?1", [id], |_| Ok(()))
                .optional()?.is_some() {
                return Ok(false);
            }
        }
        if let Some(effects) = &self.checkpoint_effects {
            if effects.observations.iter().any(|row|
                metadata.test_run_observations.get(&row.execution_id) != Some(row))
                || serde_json::to_value(&metadata.engram_source_sightings)? != effects.source_sightings {
                return Ok(false);
            }
            if let Some((id, expected)) = &effects.session {
                let stored: Option<String> = connection.query_row(
                    "SELECT value_json FROM sessions WHERE id=?1", [id], |row| row.get(0),
                ).optional()?;
                if stored.map(|row| serde_json::from_str::<Value>(&row)).transpose()?
                    .as_ref().map(checkpoint_effect_session_image).as_ref() != Some(expected) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

impl AppState {
    fn confirm_engram_containment(
        &self,
        image: EngramProjectContainmentFence,
    ) -> Result<(), ApiError> {
        let deadline = self.engram_budget_clock().now() + ENGRAM_CONTROL_SETTLE_TIMEOUT;
        self.confirm_engram_containment_until(image, deadline)
    }

    fn confirm_engram_containment_until(
        &self,
        image: EngramProjectContainmentFence,
        deadline: std::time::Instant,
    ) -> Result<(), ApiError> {
        if self.engram_budget_clock().now() >= deadline {
            return Err(ApiError::internal(
                "Engram persistence acknowledgement deadline elapsed",
            ));
        }
        let target = PersistFenceTarget::EngramProjectContainment(Box::new(image));
        let (fence, waiter) =
            PersistFence::new_with_clock(target.clone(), deadline, self.engram_budget_clock());
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
        {
            return waiter.wait_until(deadline).unwrap_or(Err(PersistFenceError::Deadline))
                .map_err(|_| ApiError::internal(
                    "Engram containment persistence is unconfirmed; affected sessions remain excluded"));
        }
        if !engram_authority_manual_writer_allowed() {
            return Err(ApiError::internal(
                "Engram containment persistence is unconfirmed because the state writer stopped; affected sessions remain excluded",
            ));
        }
        #[cfg(test)]
        {
            let mut cache = SqlitePersistConnectionCache::new();
            // Match the real writer: prove an already stored image before
            // collecting or writing a delta on idempotent accounting re-entry.
            let already_durable = cache.connection_for(self.persistence_path.as_path())
                .and_then(|connection| target.is_already_durable(connection))
                .unwrap_or(false);
            if !already_durable {
                let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
                persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta)
                    .map_err(|_| ApiError::internal("Engram containment persistence is unconfirmed; affected sessions remain excluded"))?;
            }
            // The same content proof is used even by manually driven fixtures.
            let connection = cache
                .connection_for(self.persistence_path.as_path())
                .map_err(|_| {
                    ApiError::internal("Engram containment persistence could not be verified")
                })?;
            if !target.is_already_durable(connection).map_err(|_| {
                ApiError::internal("Engram containment persistence could not be verified")
            })? {
                return Err(ApiError::conflict(
                    "Engram containment ownership changed before durability was confirmed",
                ));
            }
        }
        #[cfg(test)]
        {
            let hook = TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| hook.borrow_mut().take());
            if let Some(hook) = hook {
                hook();
            }
        }
        Ok(())
    }
}
