use super::*;

struct VerificationPages {
    binding: Option<Value>,
    pages: Mutex<std::collections::VecDeque<Value>>,
    requests: Mutex<Vec<Value>>,
}

impl EngramControlTransport for VerificationPages {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _: Duration,
    ) -> Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::WorkSessionRead { .. }) {
            return Ok(
                json!({"project_id":"established-project", "session_id":connection.session_id,
                "work_session":true, "capabilities":["host_work_v1", "verification_record_v1"]}),
            );
        }
        if let EngramControlRequest::NamedRootSightingRead {
            work_ref,
            run_id,
            run_cut,
        } = request
        {
            return Ok(scripted_no_root_sighting(
                connection, work_ref, run_id, *run_cut,
            ));
        }
        if matches!(request, EngramControlRequest::AcceptanceBindingRead { .. }) {
            return Ok(self.binding.clone().expect("unexpected binding read"));
        }
        assert!(matches!(
            request,
            EngramControlRequest::AcceptanceVerificationRead { .. }
        ));
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_value(request).unwrap());
        let page = self
            .pages
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected private page request");
        if let Some(code) = page.get("fixture_error").and_then(Value::as_str) {
            return Err(EngramTransportError::remote(EngramControlErrorBody {
                code: code.into(),
                message: "scripted private read refusal".into(),
            }));
        }
        Ok(page)
    }

    fn shutdown_session(&self, _: &str) {}
}

fn row(position: i64, source: &EngramSourceId) -> Value {
    json!({
        "record":format!("{:032x}", position), "position":position, "check_kind":"test",
        "check_fingerprint":"e".repeat(32), "result":"passed",
        "source_basis":{"workspace_id":"fixture-workspace", "source_id":source},
        "producer":{"record":format!("{:032x}", position - 1), "position":position - 1,
            "outcome":"succeeded"},
        "applicability":{"kind":"git_tree", "tree":source.tree}
    })
}

fn page(rows: Vec<Value>, earlier: usize, omitted: usize, continuation: Option<&str>) -> Value {
    let (_, _, closure) = evidence_selection::canonical_binding_receipts();
    json!({"basis":closure["basis"], "criterion":2, "requirement":{"check_kind":"test"},
        "total":earlier + rows.len() + omitted, "earlier":earlier, "shown":rows.len(),
        "omitted":omitted, "rows":rows, "continuation":continuation})
}

fn read(
    pages: Vec<Value>,
) -> (
    Result<(), ApiError>,
    AcceptanceEvaluationTask,
    Arc<VerificationPages>,
) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let transport = Arc::new(VerificationPages {
        binding: None,
        pages: Mutex::new(pages.into()),
        requests: Mutex::new(Vec::new()),
    });
    state.install_test_engram_transport(transport.clone());
    let store = established_store(&state, &project).unwrap();
    let mut discovery = state
        .acceptance_evidence_discovery_target(&parent, &store)
        .unwrap();
    discovery.identity =
        acceptance_core_identity(&evidence_selection::canonical_core_receipt()).unwrap();
    let (show, full, _) = evidence_selection::canonical_binding_receipts();
    let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
    let now = std::time::Instant::now();
    let result = state.read_acceptance_verification_evidence(
        &store,
        &mut task,
        &discovery,
        now + Duration::from_secs(120),
        &|| now,
    );
    (result, task, transport)
}

#[test]
fn evaluation_preparation_uses_private_rows_for_source_markers_without_exposing_identity() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    source::make_workdir_a_worktree(&root);
    let current = shared_source::identity();
    let captured = current.clone();
    shared_source::install_owner(&state, &project, move |_| Some(captured.clone()));
    super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "private-evidence-runtime",
    );
    let mut older = current.clone();
    older.changes[0].blob = Some("f".repeat(40));
    let (mut show, full, closure) = evidence_selection::canonical_binding_receipts();
    show["status"]["work"]["evaluation_mode"] = json!("independent_session");
    let transport = Arc::new(VerificationPages {
        binding: Some(closure),
        pages: Mutex::new(vec![page(vec![row(2, &current), row(4, &older)], 0, 0, None)].into()),
        requests: Mutex::new(Vec::new()),
    });
    state.install_test_engram_transport(transport.clone());
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            |_, args, _| {
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(evidence_selection::canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[], "omitted":0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["independent_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        )
        .unwrap();
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("independent evaluator required");
    };
    let brief = &delegation.delegation.prompt;
    for (position, matches) in [(2, true), (4, false)] {
        let locator = format!("{position:032x}");
        let lines = brief
            .lines()
            .filter(|line| line.contains(&locator) && line.contains("(verification "))
            .collect::<Vec<_>>();
        assert!(!lines.is_empty(), "the candidate verification is included");
        assert!(
            lines
                .iter()
                .all(|line| line.contains("context only") != matches),
            "only the full dirty-blob identity can distinguish the same displayed commit"
        );
    }
    assert!(!brief.contains("private-path.txt") && !brief.contains("\"source_id\""));
    assert!(
        !serde_json::to_string(&delegation)
            .unwrap()
            .contains("private-path.txt")
    );
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}

#[test]
fn private_verification_pages_keep_large_complete_identities_and_pin_every_request() {
    let mut source = shared_source::identity();
    source.changes = (0..300)
        .map(|i| {
            let mut change = source.changes[0].clone();
            change.path = format!("private-tree/file-{i:04}.txt");
            change
        })
        .collect();
    let first = row(2, &source);
    assert!(serde_json::to_vec(&first).unwrap().len() > 16 * 1024);
    let mut second_source = source.clone();
    second_source.submodules[0].commit = "f".repeat(40);
    let (result, task, transport) = read(vec![
        page(vec![first], 0, 1, Some("next-private-page")),
        page(vec![row(4, &second_source)], 1, 0, None),
    ]);
    result.unwrap();
    assert_eq!(task.indexed_evidence.len(), 2);
    for (entry, expected) in task.indexed_evidence.iter().zip([source, second_source]) {
        let metadata = entry.verification.as_ref().unwrap();
        let Some(AcceptanceSourceBasis::Shared(basis)) = &metadata.source_basis else {
            panic!("the full recorded identity must survive the private read");
        };
        assert_eq!(basis.source_id, expected);
        assert_eq!(
            metadata.applicability_tree.as_deref(),
            Some(expected.tree.as_str())
        );
        let (line, _) =
            acceptance_brief_evidence_line(entry, false, AcceptanceJudgedSource::default());
        assert!(!line.contains("private-tree/") && !line.contains("private-path.txt"));
    }
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(request["operation"], "acceptance_verification_read");
        assert_eq!(request["expected_work_revision"], 7);
        assert_eq!(request["run_cut"], 42);
        assert_eq!(request["criterion"], 2);
        assert_eq!(request["work_id"], "01a0b6c5-4b4f-7b41-9e32-edc597077acf");
        assert_eq!(request["run_id"], "01a0b6c5-4b4f-7b41-9e32-edd7dea8b369");
    }
    assert!(requests[0].get("after").is_none());
    assert_eq!(requests[1]["after"], "next-private-page");
}

#[test]
fn private_verification_pages_refuse_changed_bases_and_never_admit_partial_metadata() {
    for case in [
        "cut",
        "criterion",
        "requirement",
        "total",
        "order",
        "cursor",
        "summary",
    ] {
        let source = shared_source::identity();
        let mut first = page(vec![row(2, &source)], 0, 1, Some("private-cursor"));
        let mut second = page(vec![row(4, &source)], 1, 0, None);
        match case {
            "cut" => second["basis"]["run_cut"] = json!(43),
            "criterion" => second["criterion"] = json!(1),
            "requirement" => second["requirement"]["check_fingerprint"] = json!("f".repeat(32)),
            "total" => {
                second["total"] = json!(3);
                second["omitted"] = json!(1);
                second["continuation"] = json!("later");
            }
            "order" => second["rows"][0] = row(2, &source),
            "cursor" => {
                first["total"] = json!(3);
                first["omitted"] = json!(2);
                second["total"] = json!(3);
                second["omitted"] = json!(1);
                second["continuation"] = json!("private-cursor");
            }
            "summary" => {
                second["rows"][0]["source_basis"]["source_id"] = json!({
                "commit":source.commit, "tree":source.tree, "changes":1, "submodules":1})
            }
            _ => unreachable!(),
        }
        let (result, task, _) = read(vec![first, second]);
        assert!(result.is_err(), "{case}");
        assert!(
            task.indexed_evidence.is_empty(),
            "{case}: no prefix is usable as complete metadata"
        );
    }
}

#[test]
fn unsupported_private_verification_read_keeps_window_summaries_as_context() {
    let (result, task, _) = read(vec![json!({"fixture_error":"unsupported_operation"})]);
    result.unwrap();
    assert!(task.indexed_evidence.is_empty());
    assert!(
        task.evidence
            .iter()
            .filter_map(|entry| entry.verification.as_ref())
            .all(|metadata| metadata.source_basis.is_none())
    );
}
