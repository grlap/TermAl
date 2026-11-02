// Passive original source-sighting metadata. Its complete JSON, including old
// requests and unknown outcomes, survives without a grant, continuation,
// admission, replay, settlement or accounting implementation.

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct EngramSourceSightingOwner(Value);

impl EngramSourceSightingOwner {
    // Exact history readback only; this does not release a provider or turn.
    fn matches_metadata(&self, metadata: &PersistedState) -> bool {
        metadata.engram_source_sightings.iter().any(|stored| stored == self)
    }
}

#[cfg(test)]
mod source_sighting_history_tests {
    use super::*;

    #[test]
    fn source_sighting_history_preserves_original_json_without_runtime_ownership() {
        let original = json!({
            "scope": { "workspace_id": "historical-workspace" },
            "version": 7,
            "pending_close": { "reported_by": "historical-grant", "sighting": null },
            "observations": [{
                "id": "historical-observation",
                "phase": { "phase": "prepared", "request": {
                    "operation": "execution_observe", "routing_token": "historical-token",
                    "opaque_future_field": { "keep": true }
                } },
                "grant_settlement": null,
                "continuation_released": false
            }],
            "unknown_future_history": [null, false, { "outcome": "unknown" }]
        });
        let stored: EngramSourceSightingOwner = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&stored).unwrap(), original);
        let mut metadata = PersistedState::metadata_from_inner(&StateInner::new());
        metadata.engram_source_sightings.push(stored.clone());
        assert!(stored.matches_metadata(&metadata));
        let mut different = original;
        different["version"] = json!(8);
        let changed: EngramSourceSightingOwner = serde_json::from_value(different).unwrap();
        assert!(!changed.matches_metadata(&metadata));
    }
}
