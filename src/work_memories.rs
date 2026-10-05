// Project-memory browsing for Work. Uses the existing bounded native read
// transports and admission, never generic Engram recall/session context or
// tracker writes. Source failures remain visible, never an empty success.

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkMemoryQuery {
    search: Option<String>,
    after: Option<String>,
    key: Option<String>,
    reader_id: Option<String>,
}

impl WorkMemoryQuery {
    fn validate(&self, source: &str) -> Result<(), ApiError> {
        if source != "engram" {
            return Err(ApiError::bad_request("Unknown memory source"));
        }
        for value in [&self.search, &self.after, &self.key, &self.reader_id]
            .into_iter()
            .flatten()
        {
            if value.trim().is_empty() || value.len() > 2048 || value.chars().any(char::is_control)
            {
                return Err(ApiError::bad_request("Invalid memory query text"));
            }
        }
        if (self.key.is_some() && (self.search.is_some() || self.after.is_some()))
            || (self.search.is_some() && self.after.is_some())
            || (source == "engram"
                && (self.key.is_some() || self.after.is_some())
                && self.reader_id.is_none())
        {
            return Err(ApiError::bad_request(
                "Invalid memory query combination; Engram detail/continuation requires readerId",
            ));
        }
        Ok(())
    }

    fn arguments(&self) -> Vec<String> {
        let mut args = vec!["memories".into(), "--json".into()];
        if self.key.is_some() {
            args.push("--full".into());
        }
        if let Some(after) = &self.after {
            args.push(format!("--after={after}"));
        }
        if let Some(value) = self.key.as_ref().or(self.search.as_ref()) {
            // Positionals after -- cannot become flags, even for hostile keys.
            args.push("--".into());
            args.push(value.clone());
        }
        args
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkMemoryItem {
    key: String,
    summary: String,
    body: Option<String>,
    revision: Option<u64>,
    remembered_at: Option<String>,
    actor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkMemoryResponse {
    source: String,
    state: &'static str,
    message: String,
    items: Vec<WorkMemoryItem>,
    next_after: Option<String>,
    omitted: usize,
    exhausted: bool,
    reader_id: Option<String>,
    observed_at: String,
}

impl WorkMemoryResponse {
    fn empty(source: &str) -> Self {
        Self {
            source: source.into(),
            state: "ready",
            message: "Project memories; read-only".into(),
            items: vec![],
            next_after: None,
            omitted: 0,
            exhausted: true,
            reader_id: None,
            observed_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Deserialize)]
struct EngramMemoryRow {
    key: String,
    revision: u64,
    first_line: String,
    remembered_at: String,
    actor_id: String,
}

fn normalize_work_memories(
    source: &str,
    query: &WorkMemoryQuery,
    value: Value,
) -> Result<WorkMemoryResponse, ApiError> {
    let invalid = || ApiError::bad_gateway("Invalid project memory receipt");
    let mut response = WorkMemoryResponse::empty(source);
    if let Some(key) = &query.key {
        #[derive(Deserialize)]
        struct Full {
            key: String,
            revision: u64,
            body: String,
            remembered_at: String,
            actor_id: String,
        }
        let receipt: Full = serde_json::from_value(value).map_err(|_| invalid())?;
        if receipt.key != *key {
            return Err(invalid());
        }
        response.items.push(WorkMemoryItem {
            key: receipt.key,
            summary: receipt
                .body
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect(),
            body: Some(receipt.body),
            revision: Some(receipt.revision),
            remembered_at: Some(receipt.remembered_at),
            actor: Some(receipt.actor_id),
        });
    } else {
        #[derive(Deserialize)]
        struct Page {
            memories: Vec<EngramMemoryRow>,
            next_after: Option<String>,
            omitted_count: usize,
            exhausted: bool,
        }
        let page: Page = serde_json::from_value(value).map_err(|_| invalid())?;
        let mut keys = HashSet::new();
        if page.memories.len() > 2000
            || page
                .memories
                .iter()
                .any(|row| row.key.is_empty() || !keys.insert(&row.key))
            || (page.exhausted && (page.next_after.is_some() || page.omitted_count > 0))
            || (page.next_after.is_some()
                && (page.memories.is_empty()
                    || query.search.is_some()
                    || page.next_after == query.after))
        {
            return Err(invalid());
        }
        response.items = page
            .memories
            .into_iter()
            .map(|row| WorkMemoryItem {
                key: row.key,
                summary: row.first_line,
                body: None,
                revision: Some(row.revision),
                remembered_at: Some(row.remembered_at),
                actor: Some(row.actor_id),
            })
            .collect();
        response.next_after = page.next_after;
        response.omitted = page.omitted_count;
        response.exhausted = page.exhausted;
    }
    Ok(response)
}

impl AppState {
    fn read_work_memories<P>(
        &self,
        project_id: &str,
        source: &str,
        query: WorkMemoryQuery,
        admit: impl FnOnce() -> Result<P, ApiError>,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<WorkMemoryResponse, ApiError> {
        query.validate(source)?;
        let (project, target) = self.work_read_snapshot(project_id, query.reader_id.as_deref())?;
        let mut unavailable = WorkMemoryResponse::empty(source);
        unavailable.state = "unavailable";
        if project.remote_id != LOCAL_REMOTE_ID {
            if query.key.is_some() || query.after.is_some() {
                return Err(ApiError::conflict(
                    "Memory source changed to a remote project; refresh",
                ));
            }
            unavailable.message = "Remote project memories are not supported".into();
            return Ok(unavailable);
        }
        // Metadata-only detection before admission; follow-up reads fail hard
        // when their source vanished so clients cannot merge stale generations.
        let follow_up = query.key.is_some() || query.after.is_some();
        let read = || -> Result<WorkMemoryResponse, ApiError> {
            let target = target.map_err(ApiError::conflict)?;
            validate_work_read_target(&target)?;
            work_read_not_abandoned(cancelled)?;
            let _permit = admit()?;
            work_read_not_abandoned(cancelled)?;
            self.validate_work_read_still_current(project_id, &target)?;
            let value = run_work_read_command(
                &target.connection,
                &query.arguments(),
                &self.engram_host_launch_workdir(),
            )?;
            self.validate_work_read_still_current(project_id, &target)?;
            let mut response = normalize_work_memories(source, &query, value)?;
            response.reader_id = Some(target.reader_key);
            Ok(response)
        };
        match read() {
            Ok(response) => Ok(response),
            Err(error) if !follow_up && is_work_source_condition(error.status) => {
                unavailable.state = if error.status == StatusCode::CONFLICT {
                    "unavailable"
                } else {
                    "error"
                };
                unavailable.message = error.message;
                Ok(unavailable)
            }
            Err(error) => Err(error),
        }
    }
}

async fn get_project_work_memories(
    State(state): State<AppState>,
    AxumPath((project_id, source)): AxumPath<(String, String)>,
    limiter: Option<axum::Extension<WorkReadLimiter>>,
    query: Result<Query<WorkMemoryQuery>, QueryRejection>,
) -> Result<Json<WorkMemoryResponse>, ApiError> {
    let Query(query) = query.map_err(|e| api_query_rejection("work memories", e))?;
    query.validate(&source)?;
    let limiter = limiter.map_or_else(|| WORK_READ_PERMITS.clone(), |v| v.0.0);
    let runtime = tokio::runtime::Handle::current();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _abandoned = WorkReadAbandonGuard(cancelled.clone());
    run_blocking_api(move || {
        state
            .read_work_memories(
                &project_id,
                &source,
                query,
                || runtime.block_on(acquire_work_read_permit_from(limiter)),
                &cancelled,
            )
            .map(Json)
    })
    .await
}
