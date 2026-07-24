use super::*;

fn image_attachment_request(byte_count: usize) -> SendMessageAttachmentRequest {
    SendMessageAttachmentRequest {
        data: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            vec![0_u8; byte_count],
        ),
        file_name: Some("pasted.png".to_owned()),
        media_type: "image/png".to_owned(),
    }
}

#[test]
fn prompt_image_attachment_accepts_ten_mebibyte_boundary() {
    let parsed =
        parse_prompt_image_attachment(0, &image_attachment_request(MAX_IMAGE_ATTACHMENT_BYTES))
            .expect("a 10 MiB image should be accepted");

    assert_eq!(parsed.metadata.byte_size, 10 * 1024 * 1024);
}

#[test]
fn prompt_image_attachment_rejects_one_byte_over_ten_mebibytes() {
    let error =
        parse_prompt_image_attachment(0, &image_attachment_request(MAX_IMAGE_ATTACHMENT_BYTES + 1))
            .expect_err("an image above 10 MiB should be rejected");

    assert_eq!(error.message, "image attachment exceeds the 10 MB limit");
}

#[test]
fn json_request_body_limit_covers_base64_image_overhead() {
    let encoded_image_bytes = MAX_IMAGE_ATTACHMENT_BYTES.div_ceil(3) * 4;

    assert!(
        MAX_JSON_REQUEST_BODY_BYTES >= encoded_image_bytes + 1024,
        "the request body cap should leave room for attachment JSON metadata"
    );
}

#[tokio::test]
async fn send_message_route_accepts_ten_mebibyte_image_body() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("ten-mebibyte-image-body");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let body = serde_json::to_vec(&SendMessageRequest {
        text: String::new(),
        expanded_text: None,
        attachments: vec![image_attachment_request(MAX_IMAGE_ATTACHMENT_BYTES)],
        source_session_id: None,
        source_mailbox: None,
    })
    .expect("message body should serialize");
    assert!(body.len() > 10 * 1024 * 1024);
    assert!(body.len() < MAX_JSON_REQUEST_BODY_BYTES);

    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{session_id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request should build"),
        )
        .await
        .expect("message route should respond");

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("route should enqueue the image prompt")
    {
        ClaudeRuntimeCommand::Prompt(command) => {
            assert_eq!(command.attachments.len(), 1);
            assert_eq!(
                command.attachments[0].metadata.byte_size,
                MAX_IMAGE_ATTACHMENT_BYTES
            );
        }
        _ => panic!("expected Claude prompt command"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}
