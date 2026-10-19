// The bridge side of `termal_release_named_root`: the MCP tool definition an
// agent sees and the handler that forwards a call to
// `POST /api/sessions/{id}/engram-named-root-release` as the serving
// session, so an agent cannot name another session. Owns only the tool's text
// and argument shaping. Does not own the release, its rules or its budget
// (engram_named_root_capacity.rs, engram_named_root_release.rs), the
// allowance the bridge waits the request out for (delegation_mcp_timeouts.rs),
// or the bridge's dispatch, caller classification and tool list
// (delegation_mcp.rs, which calls both functions here).
// New module.

impl TermalDelegationMcpBridge {
    /// `termal_release_named_root`: releases this session's own retained
    /// named root of `work` at `generation`, recording `reason`.
    fn tool_release_named_root(&self, arguments: Value) -> Result<Value> {
        if let Some(fields) = arguments.as_object()
            && let Some(field) = fields
                .keys()
                .find(|field| !matches!(field.as_str(), "work" | "generation" | "reason"))
        {
            bail!("unknown field `{field}`; accepted fields are `work`, `generation` and `reason`");
        }
        let work = optional_string(arguments.get("work"))
            .filter(|work| !work.trim().is_empty())
            .ok_or_else(|| anyhow!("`work` is required: the work id or short reference"))?;
        let generation = arguments
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("`generation` is required: the root's naming generation"))?;
        let reason = optional_string(arguments.get("reason"))
            .filter(|reason| !reason.trim().is_empty())
            .ok_or_else(|| anyhow!("`reason` is required; the release audit records it"))?;
        let path = format!(
            "/api/sessions/{}/engram-named-root-release",
            self.serving_session_id
        );
        self.post_long_call(
            DelegationLongCall::NamedRootRelease,
            &path,
            &json!({ "work": work, "generation": generation, "reason": reason }),
        )
    }
}

/// `termal_release_named_root` (engram_named_root_capacity.rs).
fn named_root_release_tool_definition() -> Value {
    json!({
        "name": "termal_release_named_root",
        "description": "Release one of YOUR OWN retained named source roots that no live claim binds: a root this session named for an item it no longer works in, so it stops occupying a slot of TermAl's global named-root table. Works while you are in a turn. `work` is the work id or short reference, `generation` the root's naming generation (GET /api/engram/named-roots lists both), `reason` is recorded in the release audit. A root whose claim a session still holds is refused: clear that name with termal_name_source_root instead. A root with a canonical run association is released only on Engram's proof that its run or claim is closed. A conflict that says nothing was applied changed nothing; retry the same call.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["work", "generation", "reason"],
            "properties": {
                "work": { "type": "string" },
                "generation": { "type": "integer", "minimum": 0 },
                "reason": { "type": "string" }
            }
        }
    })
}
