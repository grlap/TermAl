// Remote-only decoding compatibility for summary fields retired by older peers.
// The canonical wire types stay strict; no discarded value reaches apply logic.
use crate::{
    CreateOrchestratorInstanceResponse, CreateProjectResponse, DeltaEvent, OrchestratorInstance,
    StateEventPayload, StateResponse, apply_mirrored_delegation_child_ids,
};
use serde::de::{DeserializeOwned, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::{RawValue, to_raw_value};
use std::fmt;

// Keep ordered raw entries, including duplicate keys, so compatibility cannot
// turn an otherwise-invalid summary into a valid one by collapsing its fields.
struct RawObject(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for RawObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = RawObject;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(RawObject(entries))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl Serialize for RawObject {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

fn discard_retired_summary_fields(raw: &mut Box<RawValue>) -> serde_json::Result<()> {
    let mut summary: RawObject = serde_json::from_str(raw.get())?;
    summary.0.retain(|(key, _)| {
        !matches!(
            key.as_str(),
            "engramControlOff" | "engramBootRecoveryPending" | "queueHold"
        )
    });
    *raw = to_raw_value(&summary)?;
    Ok(())
}

fn discard_retired_summary_array(raw: &mut Box<RawValue>) -> serde_json::Result<()> {
    let mut summaries: Vec<Box<RawValue>> = serde_json::from_str(raw.get())?;
    for summary in &mut summaries {
        discard_retired_summary_fields(summary)?;
    }
    *raw = to_raw_value(&summaries)?;
    Ok(())
}

fn decode_snapshot<T: DeserializeOwned>(payload: &str) -> serde_json::Result<T> {
    if let Ok(snapshot) = serde_json::from_str(payload) {
        return Ok(snapshot);
    }
    let mut object: RawObject = serde_json::from_str(payload)?;
    for (key, value) in &mut object.0 {
        if key == "sessions" {
            discard_retired_summary_array(value)?;
        }
    }
    serde_json::from_str(&serde_json::to_string(&object)?)
}

pub(crate) fn decode_state_event(payload: &str) -> serde_json::Result<StateEventPayload> {
    let mut event: StateEventPayload = decode_snapshot(payload)?;
    restore_mirror_classification(&mut event.state);
    Ok(event)
}

pub(crate) fn decode_delta_event(payload: &str) -> serde_json::Result<DeltaEvent> {
    let mut event = decode_delta(payload)?;
    if !event.session_summaries().is_empty() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct MirrorClassification {
            #[serde(default)]
            mirrored_delegation_child_ids: Vec<String>,
        }
        let classification: MirrorClassification = serde_json::from_str(payload)?;
        event.restore_mirror_classification(&classification.mirrored_delegation_child_ids);
    }
    Ok(event)
}

fn decode_delta(payload: &str) -> serde_json::Result<DeltaEvent> {
    // Current peers, especially streaming text deltas, keep the direct path.
    if let Ok(delta) = serde_json::from_str(payload) {
        return Ok(delta);
    }
    let mut object: RawObject = serde_json::from_str(payload)?;
    let kind = object.0.iter().find(|(key, _)| key == "type");
    let kind = kind
        .map(|(_, value)| serde_json::from_str::<String>(value.get()))
        .transpose()?;
    match kind.as_deref() {
        Some("sessionCreated") => {
            for (key, value) in &mut object.0 {
                if key == "session" {
                    discard_retired_summary_fields(value)?;
                }
            }
        }
        Some("orchestratorsUpdated") => {
            for (key, value) in &mut object.0 {
                if key == "sessions" {
                    discard_retired_summary_array(value)?;
                }
            }
        }
        _ => return serde_json::from_str(payload),
    }
    serde_json::from_str(&serde_json::to_string(&object)?)
}

fn restore_mirror_classification(state: &mut StateResponse) {
    apply_mirrored_delegation_child_ids(&mut state.sessions, &state.mirrored_delegation_child_ids);
}

// Decode every inbound remote state response inside request_json's existing
// lease checks, including snapshots nested in creation responses.
pub(crate) struct RemoteStateResponse(pub(crate) StateResponse);

impl<'de> Deserialize<'de> for RemoteStateResponse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let mut state = decode_snapshot(raw.get()).map_err(serde::de::Error::custom)?;
        restore_mirror_classification(&mut state);
        Ok(Self(state))
    }
}

pub(crate) struct RemoteCreateProjectResponse(pub(crate) CreateProjectResponse);

impl<'de> Deserialize<'de> for RemoteCreateProjectResponse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Response {
            project_id: String,
            state: RemoteStateResponse,
        }

        let response = Response::deserialize(deserializer)?;
        Ok(Self(CreateProjectResponse {
            project_id: response.project_id,
            state: response.state.0,
        }))
    }
}

pub(crate) struct RemoteCreateOrchestratorInstanceResponse(
    pub(crate) CreateOrchestratorInstanceResponse,
);

impl<'de> Deserialize<'de> for RemoteCreateOrchestratorInstanceResponse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Response {
            orchestrator: OrchestratorInstance,
            state: RemoteStateResponse,
        }

        let response = Response::deserialize(deserializer)?;
        Ok(Self(CreateOrchestratorInstanceResponse {
            orchestrator: response.orchestrator,
            state: response.state.0,
        }))
    }
}
