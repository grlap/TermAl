// Opt-in named-root wire model for scripted host tests. Grant replies remain
// scripted; this model independently checks generation and retry semantics.
#[derive(Default)]
struct TestEngramNamedRoots {
    bindings: BTreeMap<String, String>,
    runs: BTreeMap<String, String>,
    read_bindings: BTreeMap<String, EngramControlWorkBinding>,
    run_generations: BTreeMap<String, i64>,
    run_cuts: BTreeMap<String, i64>,
    project_id: Option<String>,
    latest: BTreeMap<String, Value>,
    released: BTreeMap<String, i64>,
    seen: BTreeMap<(String, String), (Value, Value)>,
    lose_next_reply: bool,
    refuse_next: bool,
    fail_next_status: bool,
    root_reads: BTreeMap<String, Value>,
    root_read_failures: BTreeSet<String>,
    // Model an immutable pre-field receipt without changing current bind/status.
    omitted_begin_projections: BTreeSet<String>,
    scripted_status: bool,
}

impl TestEngramNamedRoots {
    fn register(&mut self, binding: &EngramControlWorkBinding) {
        self.runs
            .insert(binding.claim_id.clone(), binding.run_id.clone());
        if !self.run_generations.contains_key(&binding.run_id) {
            let next = self
                .read_bindings
                .values()
                .filter(|previous| previous.work_id == binding.work_id)
                .filter_map(|previous| self.run_generations.get(&previous.run_id))
                .copied()
                .max()
                .unwrap_or(0)
                + 1;
            self.run_generations.insert(binding.run_id.clone(), next);
        }
        self.read_bindings
            .insert(binding.claim_id.clone(), binding.clone());
    }

    fn state(&self, session: &str) -> Value {
        if let Some(generation) = self
            .bindings
            .get(session)
            .and_then(|claim| self.released.get(claim))
        {
            return json!({"state":"unbound_by_release", "last_generation":generation, "released_at_position":100});
        }
        self.bindings
            .get(session)
            .and_then(|claim| self.latest.get(claim))
            .map_or(json!({"state":"none"}), |event| {
                if event["kind"] == "ended" {
                    json!({"state":"none"})
                } else {
                    let named_at =
                        chrono::DateTime::parse_from_rfc3339(event["named_at"].as_str().unwrap())
                            .unwrap()
                            .with_timezone(&chrono::Utc)
                            .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
                    json!({"state":"bound", "workspace_id":event["workspace_id"],
                    "generation":event["generation"], "named_at":named_at})
                }
            })
    }

    fn request(
        &mut self,
        session: &str,
        request: &EngramControlRequest,
    ) -> Option<Result<Value, EngramTransportError>> {
        let mut wire = serde_json::to_value(request).unwrap();
        if wire["operation"] == "named_root_read" {
            if self
                .root_read_failures
                .contains(wire["claim_id"].as_str().unwrap())
            {
                return Some(Err(EngramTransportError::transport(
                    "claim reader timed out",
                )));
            }
            let claim = wire["claim_id"].as_str().unwrap();
            if let Some(read) = self.root_reads.get(claim) {
                if let (Some(run), Some(cut)) = (
                    read["run_id"].as_str(),
                    read["read_cut"]["position"].as_i64(),
                ) {
                    let entry = self.run_cuts.entry(run.to_owned()).or_default();
                    *entry = (*entry).max(cut);
                }
                return Some(Ok(read.clone()));
            }
            let Some(binding) = self.read_bindings.get(claim) else {
                return Some(Err(EngramTransportError::transport(
                    "claim reader unavailable",
                )));
            };
            let latest = self.latest.get(claim);
            let root = latest.map_or(json!({"state":"none"}), |latest| {
                if let Some(generation) = self.released.get(claim) {
                    json!({"state":"unbound_by_release", "last_generation":generation, "released_at_position":100})
                } else if latest["kind"] == "ended" {json!({"state":"none"})}
                else {json!({"state":"bound", "workspace_id":latest["workspace_id"],
                    "generation":latest["generation"], "named_at":latest["named_at"]})}
            });
            let position =
                latest.map_or(0, |event| event["position"]["position"].as_i64().unwrap());
            let cut = self.run_cuts.entry(binding.run_id.clone()).or_default();
            *cut = (*cut).max(if self.released.contains_key(claim) {
                100
            } else {
                position
            });
            return Some(Ok(
                json!({"project_id":self.project_id.as_deref().unwrap_or("github.com/example/source-root"), "work_id":binding.work_id,
                    "root_execution_id":binding.root_execution_id,"run_id":binding.run_id,"claim_id":binding.claim_id,
                    "run":{"state":"open", "generation":self.run_generations[&binding.run_id]},
                    "named_root":root, "latest_event":latest.map(|event| json!({"event":event["event"],
                        "position":event["position"],"kind":event["kind"],"generation":event["generation"],
                        "workspace_id":event["workspace_id"],"named_at":event["named_at"]})),
                    "read_cut":{"feed":{"kind":"run_execution", "id":binding.run_id},
                        "position":*cut}
                }),
            ));
        }
        if wire["operation"] == "session_status" && !self.scripted_status {
            if std::mem::take(&mut self.fail_next_status) {
                return Some(Err(EngramTransportError::transport(
                    "status reply unavailable",
                )));
            }
            return Some(Ok(
                json!({"phase":"ready", "named_root":self.state(session)}),
            ));
        }
        if wire["operation"] == "session_bind" {
            if let Some(claim) = wire["work_binding"]["claim_id"].as_str() {
                self.bindings.insert(session.to_owned(), claim.to_owned());
                self.register(&serde_json::from_value(wire["work_binding"].clone()).unwrap());
            } else {
                self.bindings.remove(session);
            }
        }
        if wire["operation"] != "named_root_bind" {
            return None;
        }
        wire.as_object_mut().unwrap().remove("routing_token");
        let key = (
            session.to_owned(),
            wire["idempotency_key"].as_str().unwrap().to_owned(),
        );
        if let Some((intent, receipt)) = self.seen.get(&key) {
            return Some(if intent == &wire {
                Ok(receipt.clone())
            } else {
                Err(EngramTransportError::remote(EngramControlErrorBody {
                    code: "control_operation_idempotency_conflict".to_owned(),
                    message: "intent changed".to_owned(),
                }))
            });
        }
        let claim = wire["claim_id"].as_str().unwrap().to_owned();
        let generation = wire["generation"].as_i64().unwrap();
        let Some(run) = self.runs.get(&claim) else {
            return Some(Err(EngramTransportError::remote(EngramControlErrorBody {
                code: "named_root_binding_refused".to_owned(),
                message: "claim has no registered run".to_owned(),
            })));
        };
        let previous = self.latest.get(&claim);
        let valid = generation > 0
            && if wire["kind"] == "bound" {
                wire.get("end_reason").is_none()
                    && previous.is_none_or(|p| generation > p["generation"].as_i64().unwrap())
            } else {
                previous.is_some_and(|p| {
                    p["kind"] == "bound"
                        && wire["generation"] == p["generation"]
                        && wire["workspace_id"] == p["workspace_id"]
                        && wire["named_at"] == p["named_at"]
                        && (wire["end_reason"] != "explicit_clear" || p["reporter"] == session)
                })
            };
        if !valid || std::mem::take(&mut self.refuse_next) {
            return Some(Err(EngramTransportError::remote(EngramControlErrorBody {
                code: "named_root_binding_refused".to_owned(),
                message: "invalid root transition".to_owned(),
            })));
        }
        let next_position = self
            .run_cuts
            .get(run)
            .copied()
            .unwrap_or(0)
            .max(self.seen.len() as i64)
            + 1;
        self.run_cuts.insert(run.clone(), next_position);
        let receipt = json!({"event":format!("event-{}", self.seen.len()+1),
            "position":{"feed":{"kind":"run_execution", "id":run},
                "position":next_position}, "workspace_id":wire["workspace_id"],
            "generation":generation, "kind":wire["kind"]});
        self.seen.insert(key, (wire.clone(), receipt.clone()));
        self.released.remove(&claim);
        wire["reporter"] = json!(session);
        wire["event"] = receipt["event"].clone();
        wire["position"] = receipt["position"].clone();
        self.latest.insert(claim.clone(), wire);
        self.root_reads.remove(&claim);
        Some(if std::mem::take(&mut self.lose_next_reply) {
            Err(EngramTransportError::transport(
                "fixture lost reply after durable event",
            ))
        } else {
            Ok(receipt)
        })
    }

    fn annotate(&self, session: &str, request: &EngramControlRequest, reply: &mut Value) {
        if !self.bindings.contains_key(session) {
            return; // Real unbound receipts omit the claimed-root projection.
        }
        match request {
            EngramControlRequest::SessionBind { .. } => {
                reply["status"]["named_root"] = self.state(session)
            }
            EngramControlRequest::TurnBegin { grant_id, .. }
                if !self.omitted_begin_projections.contains(grant_id) =>
            {
                reply["receipt"]["named_root"] = self.state(session)
            }
            EngramControlRequest::SessionStatus { .. } => reply["named_root"] = self.state(session),
            _ => {}
        }
    }
}

impl ScriptedEngramControlTransport {
    fn register_named_root_run(&self, binding: &EngramControlWorkBinding) {
        self.named_roots
            .lock()
            .unwrap()
            .get_or_insert_with(TestEngramNamedRoots::default)
            .register(binding);
    }

    fn enable_named_roots(&self, session: &str, binding: &EngramControlWorkBinding) {
        let mut roots = self.named_roots.lock().unwrap();
        let roots = roots.get_or_insert_with(TestEngramNamedRoots::default);
        roots
            .bindings
            .insert(session.to_owned(), binding.claim_id.clone());
        roots.register(binding);
    }
}
