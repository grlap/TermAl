// The bridge side of `termal_name_source_root` (tm-5gi4 phase 2): the MCP tool
// definition an agent sees and the handler that forwards a call to
// `POST /api/sessions/{id}/engram-source-root`. Owns only the tool's text,
// argument shaping and the request's timeout. Does not own naming itself, its
// validation or its budget (`engram_source_roots.rs`), or the bridge's
// dispatch, caller classification and tool list (`delegation_mcp.rs`, which
// calls both functions here). Split out of `delegation_mcp.rs`, which is
// already past its size limit (tm-dy69), so the phase adds only its dispatch
// and tool-list entries there.

impl TermalDelegationMcpBridge {
    /// `termal_name_source_root` (engram_source_roots.rs): names, or with no
    /// `path` clears, the source root of a work this session holds a claim on.
    fn tool_name_source_root(&self, arguments: Value) -> Result<Value> {
        let work = optional_string(arguments.get("work"))
            .filter(|work| !work.trim().is_empty())
            .ok_or_else(|| anyhow!("`work` is required: the work id or short reference"))?;
        let mut body = serde_json::Map::new();
        body.insert("work".to_owned(), Value::String(work));
        // Only an omitted `path` clears: a present one is sent as written, so
        // the server refuses a blank one rather than read it as a clear.
        match arguments.get("path") {
            None => {}
            Some(Value::String(path)) => {
                body.insert("path".to_owned(), Value::String(path.clone()));
            }
            Some(_) => bail!(
                "`path` must be a string naming the worktree; omit it to clear the name"
            ),
        }
        // The server bounds naming by its own budget; waiting that on top of
        // the normal allowance means a reported failure is never a name the
        // server kept anyway.
        let path = format!("/api/sessions/{}/engram-source-root", self.serving_session_id);
        self.decode_response(
            "POST",
            &path,
            self.client
                .post(self.url(&path))
                .timeout(ENGRAM_SOURCE_ROOT_NAMING_BUDGET + self.request_timeout)
                .json(&Value::Object(body))
                .send(),
        )
    }
}

/// `termal_name_source_root` (engram_source_roots.rs, tm-5gi4 phase 2).
fn source_root_tool_definition() -> Value {
    json!({
        "name": "termal_name_source_root",
        "description": "Name the source root of an Engram work item you hold a live claim on: the git worktree where you do that item's work. From your next turn, TermAl measures that worktree instead of your session's folder for every source revision of your turns on the claim, credits only tests that run in it, and runs the acceptance evaluator there with its declared fingerprint. `work` is the work id or its short reference. `path` is the worktree's root, absolute or relative to your session's folder; it must be a registered worktree of this repository inside the project folder, on a local drive rather than a network share. Omit `path` to clear the name. Name it before your first edit for the item. At landing, request the evaluation while the worktree still exists, then remove it: a named root that is gone is not measured and declares no revision, and TermAl never falls back to the main checkout. Naming the main checkout takes the shared tree on your item. A rename or a clear seals the old root's revision for your turn running now in it on that claim; a sealed revision does not know of edits made after the seal.",
        "inputSchema": {
            "type": "object",
            "required": ["work"],
            "properties": {
                "work": { "type": "string" },
                "path": { "type": "string" }
            }
        }
    })
}
