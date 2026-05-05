// Conversation marker CRUD.
//
// Markers are lightweight session metadata anchored to message ids. The
// transcript remains the source of truth for geometry; markers only store
// stable anchors plus index hints so clients can render quickly and recover
// after compaction or reload.

const CONVERSATION_MARKER_NAME_MAX_CHARS: usize = 120;
const CONVERSATION_MARKER_BODY_MAX_CHARS: usize = 4_000;

impl AppState {
    fn list_conversation_markers(
        &self,
        session_id: &str,
    ) -> Result<ConversationMarkersResponse, ApiError> {
        let session_id = normalize_marker_route_id(session_id, "session id")?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .ok_or_else(ApiError::local_session_missing)?;
        let record = inner
            .sessions
            .get(index)
            .ok_or_else(ApiError::local_session_missing)?;
        Ok(ConversationMarkersResponse {
            markers: record.session.markers.clone(),
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn create_conversation_marker(
        &self,
        session_id: &str,
        request: CreateConversationMarkerRequest,
    ) -> Result<ConversationMarkerResponse, ApiError> {
        let session_id = normalize_marker_route_id(session_id, "session id")?;
        if self.remote_session_target(&session_id)?.is_some() {
            return self.proxy_remote_create_conversation_marker(&session_id, request);
        }

        let (marker, revision, session_mutation_stamp) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let marker = {
                let record = inner
                    .session_by_index(index)
                    .expect("session index should be valid");
                let marker = build_conversation_marker(record, request)?;
                ensure_local_marker_session(record)?;
                marker
            };
            let session_mutation_stamp = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                record.session.markers.push(marker.clone());
                record.mutation_stamp
            };
            let revision = self.commit_persisted_delta_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist conversation marker: {err:#}"))
            })?;
            (marker, revision, session_mutation_stamp)
        };
        self.publish_delta(&DeltaEvent::ConversationMarkerCreated {
            revision,
            session_id: session_id.clone(),
            marker: marker.clone(),
            session_mutation_stamp: Some(session_mutation_stamp),
        });
        Ok(ConversationMarkerResponse {
            marker,
            revision,
            server_instance_id: self.server_instance_id.clone(),
            session_mutation_stamp: Some(session_mutation_stamp),
        })
    }

    fn update_conversation_marker(
        &self,
        session_id: &str,
        marker_id: &str,
        request: UpdateConversationMarkerRequest,
    ) -> Result<ConversationMarkerResponse, ApiError> {
        let session_id = normalize_marker_route_id(session_id, "session id")?;
        let marker_id = normalize_marker_route_id(marker_id, "marker id")?;
        if !update_conversation_marker_request_has_changes(&request) {
            return Err(ApiError::bad_request(
                "conversation marker update must include at least one field",
            ));
        }
        if self.remote_session_target(&session_id)?.is_some() {
            return self.proxy_remote_update_conversation_marker(&session_id, &marker_id, request);
        }

        let (marker, revision, session_mutation_stamp) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let marker = {
                let record = inner
                    .session_by_index(index)
                    .expect("session index should be valid");
                ensure_local_marker_session(record)?;
                let marker_index = record
                    .session
                    .markers
                    .iter()
                    .position(|marker| marker.id == marker_id)
                    .ok_or_else(|| ApiError::not_found("conversation marker not found"))?;
                let next_marker =
                    patch_conversation_marker(record, marker_index, request, &session_id)?;
                next_marker
            };
            let session_mutation_stamp = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let marker_index = record
                    .session
                    .markers
                    .iter()
                    .position(|entry| entry.id == marker_id)
                    .ok_or_else(|| ApiError::not_found("conversation marker not found"))?;
                record.session.markers[marker_index] = marker.clone();
                record.mutation_stamp
            };
            let revision = self.commit_persisted_delta_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist conversation marker: {err:#}"))
            })?;
            (marker, revision, session_mutation_stamp)
        };
        self.publish_delta(&DeltaEvent::ConversationMarkerUpdated {
            revision,
            session_id: session_id.clone(),
            marker: marker.clone(),
            session_mutation_stamp: Some(session_mutation_stamp),
        });
        Ok(ConversationMarkerResponse {
            marker,
            revision,
            server_instance_id: self.server_instance_id.clone(),
            session_mutation_stamp: Some(session_mutation_stamp),
        })
    }

    fn delete_conversation_marker(
        &self,
        session_id: &str,
        marker_id: &str,
    ) -> Result<DeleteConversationMarkerResponse, ApiError> {
        let session_id = normalize_marker_route_id(session_id, "session id")?;
        let marker_id = normalize_marker_route_id(marker_id, "marker id")?;
        if self.remote_session_target(&session_id)?.is_some() {
            return self.proxy_remote_delete_conversation_marker(&session_id, &marker_id);
        }

        let (revision, session_mutation_stamp) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            {
                let record = inner
                    .session_by_index(index)
                    .expect("session index should be valid");
                ensure_local_marker_session(record)?;
                if !record
                    .session
                    .markers
                    .iter()
                    .any(|marker| marker.id == marker_id)
                {
                    return Err(ApiError::not_found("conversation marker not found"));
                }
            }
            let session_mutation_stamp = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let marker_index = record
                    .session
                    .markers
                    .iter()
                    .position(|marker| marker.id == marker_id)
                    .ok_or_else(|| ApiError::not_found("conversation marker not found"))?;
                record.session.markers.remove(marker_index);
                record.mutation_stamp
            };
            let revision = self.commit_persisted_delta_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to delete conversation marker: {err:#}"))
            })?;
            (revision, session_mutation_stamp)
        };
        self.publish_delta(&DeltaEvent::ConversationMarkerDeleted {
            revision,
            session_id: session_id.clone(),
            marker_id: marker_id.clone(),
            session_mutation_stamp: Some(session_mutation_stamp),
        });
        Ok(DeleteConversationMarkerResponse {
            marker_id,
            revision,
            server_instance_id: self.server_instance_id.clone(),
            session_mutation_stamp: Some(session_mutation_stamp),
        })
    }

    fn proxy_remote_create_conversation_marker(
        &self,
        session_id: &str,
        request: CreateConversationMarkerRequest,
    ) -> Result<ConversationMarkerResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_response: ConversationMarkerResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/markers",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            Some(serde_json::to_value(request).map_err(|err| {
                ApiError::internal(format!("failed to encode marker create request: {err}"))
            })?),
        )?;
        self.apply_remote_marker_response(target, remote_response, true, None)
    }

    fn proxy_remote_update_conversation_marker(
        &self,
        session_id: &str,
        marker_id: &str,
        request: UpdateConversationMarkerRequest,
    ) -> Result<ConversationMarkerResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_response: ConversationMarkerResponse = self.remote_registry.request_json(
            &target.remote,
            Method::PATCH,
            &format!(
                "/api/sessions/{}/markers/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(marker_id)
            ),
            &[],
            Some(serde_json::to_value(request).map_err(|err| {
                ApiError::internal(format!("failed to encode marker update request: {err}"))
            })?),
        )?;
        self.apply_remote_marker_response(target, remote_response, false, Some(marker_id))
    }

    fn proxy_remote_delete_conversation_marker(
        &self,
        session_id: &str,
        marker_id: &str,
    ) -> Result<DeleteConversationMarkerResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_response: DeleteConversationMarkerResponse = self.remote_registry.request_json(
            &target.remote,
            Method::DELETE,
            &format!(
                "/api/sessions/{}/markers/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(marker_id)
            ),
            &[],
            None,
        )?;
        if remote_response.marker_id != marker_id {
            return Err(ApiError::bad_gateway(format!(
                "remote deleted marker id `{}` did not match requested marker `{marker_id}`",
                remote_response.marker_id
            )));
        }
        self.apply_remote_marker_delete_response(target, remote_response)
    }

    fn apply_remote_marker_response(
        &self,
        target: RemoteSessionTarget,
        remote_response: ConversationMarkerResponse,
        created: bool,
        expected_marker_id: Option<&str>,
    ) -> Result<ConversationMarkerResponse, ApiError> {
        if remote_response.marker.session_id != target.remote_session_id {
            return Err(ApiError::bad_gateway(format!(
                "remote marker payload session id `{}` did not match requested session `{}`",
                remote_response.marker.session_id, target.remote_session_id
            )));
        }
        // Create responses carry a remote-assigned marker id. Only update
        // responses must match the path-bound marker id.
        if let Some(expected_marker_id) = expected_marker_id {
            if remote_response.marker.id != expected_marker_id {
                return Err(ApiError::bad_gateway(format!(
                    "remote updated marker id `{}` did not match requested marker `{expected_marker_id}`",
                    remote_response.marker.id
                )));
            }
        }
        let remote_revision = remote_response.revision;
        let remote_session_mutation_stamp = remote_response.session_mutation_stamp;
        let marker_id = remote_response.marker.id.clone();
        let event = if created {
            DeltaEvent::ConversationMarkerCreated {
                revision: remote_revision,
                session_id: target.remote_session_id.clone(),
                marker: remote_response.marker,
                session_mutation_stamp: remote_session_mutation_stamp,
            }
        } else {
            DeltaEvent::ConversationMarkerUpdated {
                revision: remote_revision,
                session_id: target.remote_session_id.clone(),
                marker: remote_response.marker,
                session_mutation_stamp: remote_session_mutation_stamp,
            }
        };
        self.apply_remote_delta_event(&target.remote.id, event)
        .map_err(|err| {
            ApiError::bad_gateway(format!("failed to apply remote marker response: {err:#}"))
        })?;

        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&target.local_session_id)
            .ok_or_else(ApiError::local_session_missing)?;
        let record = inner
            .session_by_index(index)
            .ok_or_else(ApiError::local_session_missing)?;
        let marker = record
            .session
            .markers
            .iter()
            .find(|marker| marker.id == marker_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("conversation marker not found"))?;
        Ok(ConversationMarkerResponse {
            marker,
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
            session_mutation_stamp: Some(record.mutation_stamp),
        })
    }

    fn apply_remote_marker_delete_response(
        &self,
        target: RemoteSessionTarget,
        remote_response: DeleteConversationMarkerResponse,
    ) -> Result<DeleteConversationMarkerResponse, ApiError> {
        let marker_id = remote_response.marker_id.clone();
        let remote_session_mutation_stamp = remote_response.session_mutation_stamp;
        let remote_revision = remote_response.revision;
        let event = DeltaEvent::ConversationMarkerDeleted {
            revision: remote_revision,
            session_id: target.remote_session_id.clone(),
            marker_id: marker_id.clone(),
            session_mutation_stamp: remote_session_mutation_stamp,
        };
        self.apply_remote_delta_event(&target.remote.id, event).map_err(|err| {
            ApiError::bad_gateway(format!("failed to apply remote marker delete response: {err:#}"))
        })?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let session_mutation_stamp = inner
            .find_session_index(&target.local_session_id)
            .and_then(|index| inner.sessions.get(index))
            .map(|record| record.mutation_stamp);
        Ok(DeleteConversationMarkerResponse {
            marker_id,
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
            session_mutation_stamp,
        })
    }
}

fn build_conversation_marker(
    record: &SessionRecord,
    request: CreateConversationMarkerRequest,
) -> Result<ConversationMarker, ApiError> {
    let session_id = record.session.id.clone();
    let name = normalize_conversation_marker_name(&request.name)?;
    let body = normalize_conversation_marker_body(request.body)?;
    let color = normalize_conversation_marker_color(&request.color)?;
    let message_id = normalize_marker_route_id(&request.message_id, "message id")?;
    let message_index_hint = message_index_on_session(&record.session, &message_id)
        .ok_or_else(|| ApiError::bad_request("conversation marker message id was not found"))?;
    let (end_message_id, end_message_index_hint) = resolve_marker_end_anchor(
        record,
        message_index_hint,
        request.end_message_id.as_deref(),
    )?;
    let now = stamp_now();
    Ok(ConversationMarker {
        id: format!("marker-{}", Uuid::new_v4()),
        session_id,
        kind: request.kind,
        name,
        body,
        color,
        message_id,
        message_index_hint,
        end_message_id,
        end_message_index_hint,
        created_at: now.clone(),
        updated_at: now,
        created_by: ConversationMarkerAuthor::User,
    })
}

fn patch_conversation_marker(
    record: &SessionRecord,
    marker_index: usize,
    request: UpdateConversationMarkerRequest,
    session_id: &str,
) -> Result<ConversationMarker, ApiError> {
    let mut marker = record
        .session
        .markers
        .get(marker_index)
        .cloned()
        .ok_or_else(|| ApiError::not_found("conversation marker not found"))?;

    if let Some(kind) = request.kind {
        marker.kind = kind;
    }
    if let Some(name) = request.name {
        marker.name = normalize_conversation_marker_name(&name)?;
    }
    if let Some(body) = request.body {
        marker.body = normalize_conversation_marker_body(body)?;
    }
    if let Some(color) = request.color {
        marker.color = normalize_conversation_marker_color(&color)?;
    }
    if let Some(message_id) = request.message_id {
        let message_id = normalize_marker_route_id(&message_id, "message id")?;
        let message_index_hint = message_index_on_session(&record.session, &message_id)
            .ok_or_else(|| ApiError::bad_request("conversation marker message id was not found"))?;
        marker.message_id = message_id;
        marker.message_index_hint = message_index_hint;
    }
    if let Some(end_message_id) = request.end_message_id {
        match end_message_id {
            Some(end_message_id) => {
                let (end_message_id, end_message_index_hint) = resolve_marker_end_anchor(
                    record,
                    marker.message_index_hint,
                    Some(end_message_id.as_str()),
                )?;
                marker.end_message_id = end_message_id;
                marker.end_message_index_hint = end_message_index_hint;
            }
            None => {
                marker.end_message_id = None;
                marker.end_message_index_hint = None;
            }
        }
    } else if let Some(end_message_id) = marker.end_message_id.clone() {
        let (_end_message_id, end_message_index_hint) =
            resolve_marker_end_anchor(record, marker.message_index_hint, Some(&end_message_id))?;
        marker.end_message_index_hint = end_message_index_hint;
    }
    marker.session_id = session_id.to_owned();
    marker.updated_at = stamp_now();

    Ok(marker)
}

fn resolve_marker_end_anchor(
    record: &SessionRecord,
    start_message_index: usize,
    end_message_id: Option<&str>,
) -> Result<(Option<String>, Option<usize>), ApiError> {
    let Some(end_message_id) = end_message_id else {
        return Ok((None, None));
    };
    let end_message_id = normalize_marker_route_id(end_message_id, "end message id")?;
    let end_message_index = message_index_on_session(&record.session, &end_message_id)
        .ok_or_else(|| ApiError::bad_request("conversation marker end message id was not found"))?;
    if end_message_index < start_message_index {
        return Err(ApiError::bad_request(
            "conversation marker end message must not precede the start message",
        ));
    }
    Ok((Some(end_message_id), Some(end_message_index)))
}

fn message_index_on_session(session: &Session, message_id: &str) -> Option<usize> {
    session
        .messages
        .iter()
        .position(|message| message.id() == message_id)
}

fn update_conversation_marker_request_has_changes(request: &UpdateConversationMarkerRequest) -> bool {
    request.kind.is_some()
        || request.name.is_some()
        || request.body.is_some()
        || request.color.is_some()
        || request.message_id.is_some()
        || request.end_message_id.is_some()
}

fn normalize_marker_route_id(value: &str, label: &str) -> Result<String, ApiError> {
    normalize_optional_identifier(Some(value))
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request(format!("{label} is required")))
}

fn normalize_conversation_marker_name(value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("conversation marker name is required"));
    }
    if trimmed.chars().count() > CONVERSATION_MARKER_NAME_MAX_CHARS {
        return Err(ApiError::bad_request(format!(
            "conversation marker name must be at most {CONVERSATION_MARKER_NAME_MAX_CHARS} characters"
        )));
    }
    Ok(trimmed.to_owned())
}

fn normalize_conversation_marker_body(value: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > CONVERSATION_MARKER_BODY_MAX_CHARS {
        return Err(ApiError::bad_request(format!(
            "conversation marker body must be at most {CONVERSATION_MARKER_BODY_MAX_CHARS} characters"
        )));
    }
    Ok(Some(trimmed.to_owned()))
}

const CONVERSATION_MARKER_COLOR_ERROR: &str =
    "conversation marker color must be a 3, 4, 6, or 8 digit hex color";

fn normalize_conversation_marker_color(value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if !is_valid_conversation_marker_color(trimmed) {
        return Err(ApiError::bad_request(CONVERSATION_MARKER_COLOR_ERROR));
    }
    Ok(trimmed.to_ascii_lowercase())
}

fn is_valid_conversation_marker_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|character| character.is_ascii_hexdigit())
}

fn ensure_local_marker_session(record: &SessionRecord) -> Result<(), ApiError> {
    if record.remote_id.is_some() {
        return Err(ApiError::bad_request(
            "conversation markers on remote proxy sessions are read-only on this host",
        ));
    }
    Ok(())
}
