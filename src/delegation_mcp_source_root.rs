// The bridge side of `termal_name_source_root`: the MCP tool definition an
// agent sees and the handler that forwards a call to
// `POST /api/sessions/{id}/engram-source-root`. Owns only the tool's text and
// argument shaping. Does not own naming itself, its validation or its budget
// (`engram_source_roots.rs`), the allowance the bridge waits the request out
// for (`delegation_mcp_timeouts.rs`), or the bridge's dispatch, caller
// classification and tool list (`delegation_mcp.rs`, which calls both
// functions here). Split out of `delegation_mcp.rs`, which is already past
// its size limit, so this tool adds only its dispatch and tool-list entries
// there.

impl TermalDelegationMcpBridge {
    /// `termal_name_source_root` (engram_source_roots.rs): names, or with no
    /// `path` clears, the source root of a work this session holds a claim on.
    fn tool_name_source_root(&self, arguments: Value) -> Result<Value> {
        // Refuse before shaping the body: dropping a misspelled `path`
        // would turn a naming request into the documented omission-clear.
        if let Some(fields) = arguments.as_object()
            && let Some(field) = fields
                .keys()
                .find(|field| !matches!(field.as_str(), "work" | "path"))
        {
            bail!("unknown field `{field}`; accepted fields are `work` and `path`");
        }
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
        // the normal request timeout means a reported failure is never a name
        // the server kept anyway.
        let path = format!("/api/sessions/{}/engram-source-root", self.serving_session_id);
        self.post_long_call(
            DelegationLongCall::SourceRootNaming,
            &path,
            &Value::Object(body),
        )
    }
}

/// `termal_name_source_root` (engram_source_roots.rs).
fn source_root_tool_definition() -> Value {
    json!({
        "name": "termal_name_source_root",
        "description": "Name the source root of an Engram work item you hold a live claim on: the git worktree where you do that item's work. From your next turn, TermAl measures that worktree instead of your session's folder for every source revision of your turns on the claim, credits only tests that run in it, and runs the acceptance evaluator there with its declared fingerprint. `work` is the work id or its short reference. `path` is the worktree's root, absolute or relative to your session's folder; it must be a registered worktree of this repository inside the project folder, on a local drive rather than a network share. Omit `path` to clear the name. Unknown fields are refused; accepted fields are `work` and `path`. Name it before your first edit for the item. At landing, request the evaluation while the worktree still exists, then remove it: a named root that is gone is not measured and declares no revision, and TermAl never falls back to the main checkout. Naming the main checkout takes the shared tree on your item. A rename or a clear seals the old root's revision for your turn running now in it on that claim; a sealed revision does not know of edits made after the seal.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["work"],
            "properties": {
                "work": { "type": "string" },
                "path": { "type": "string" }
            }
        }
    })
}
