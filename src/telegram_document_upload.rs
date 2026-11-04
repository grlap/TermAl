/*
Telegram `sendDocument` upload.

Owns: the `TelegramDocumentSender` seam that tests replace with a fake, the
production implementation on `TelegramApiClient`, and the hand-built
`multipart/form-data` body Telegram requires for a file upload. The body is
built here because the crate's reqwest build has no `multipart` feature.

Deliberately does not own: file admission (`telegram_document_files.rs`), link
state or the HTTP route (`telegram_documents.rs`), or the shared JSON
request/envelope decoding (`telegram_clients.rs`).

Secrets: the bot token stays inside `TelegramApiClient::api_base_url`. Errors
that leave this module still carry that URL inside reqwest's text, so callers
pass them through `sanitize_telegram_log_detail` before logging or returning
them.

Provenance: new module; nothing was moved here from another file.
*/

/// One admitted Markdown document plus its optional plain-text caption.
struct TelegramDocumentUpload<'a> {
    file_name: &'a str,
    bytes: &'a [u8],
    caption: Option<&'a str>,
}

/// Narrow `sendDocument` result projection: only the message id is used.
#[derive(Debug, Deserialize)]
struct TelegramSentDocumentMessage {
    message_id: i64,
}

trait TelegramDocumentSender {
    /// Sends the document to `chat_id` and returns the Telegram message id.
    fn send_document(&self, chat_id: i64, upload: &TelegramDocumentUpload<'_>) -> Result<i64>;
}

impl TelegramDocumentSender for TelegramApiClient {
    fn send_document(&self, chat_id: i64, upload: &TelegramDocumentUpload<'_>) -> Result<i64> {
        let method = "sendDocument";
        let (content_type, body) = telegram_document_multipart_body(chat_id, upload);
        let url = format!("{}/{}", self.api_base_url, method);
        let response = self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .with_context(|| format!("failed to call Telegram `{method}`"))?;
        let sent: TelegramSentDocumentMessage = decode_telegram_api_response(method, response)?;
        Ok(sent.message_id)
    }
}

/// Builds a `multipart/form-data` body with `chat_id`, optional `caption`,
/// and the `document` file part. Returns the Content-Type header value (with
/// its boundary) and the body bytes.
fn telegram_document_multipart_body(
    chat_id: i64,
    upload: &TelegramDocumentUpload<'_>,
) -> (String, Vec<u8>) {
    let boundary = telegram_document_multipart_boundary(upload);
    let mut body = Vec::with_capacity(upload.bytes.len() + 512);
    let push_text_part = |body: &mut Vec<u8>, name: &str, value: &str| {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    };
    push_text_part(&mut body, "chat_id", &chat_id.to_string());
    if let Some(caption) = upload.caption {
        push_text_part(&mut body, "caption", caption);
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"document\"; filename=\"{}\"\r\nContent-Type: text/markdown; charset=utf-8\r\n\r\n",
            telegram_document_multipart_file_name(upload.file_name)
        )
        .as_bytes(),
    );
    body.extend_from_slice(upload.bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Picks a random boundary that occurs in neither the caption nor the file.
fn telegram_document_multipart_boundary(upload: &TelegramDocumentUpload<'_>) -> String {
    loop {
        let boundary = format!("termal-{}", Uuid::new_v4().simple());
        let in_caption = upload
            .caption
            .is_some_and(|caption| caption.contains(&boundary));
        let in_file = upload
            .bytes
            .windows(boundary.len())
            .any(|window| window == boundary.as_bytes());
        if !in_caption && !in_file {
            return boundary;
        }
    }
}

/// Keeps the quoted `filename` parameter well-formed: quotes, backslashes and
/// control characters would break or inject into the part headers.
fn telegram_document_multipart_file_name(file_name: &str) -> String {
    file_name
        .chars()
        .map(|ch| {
            if ch == '"' || ch == '\\' || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .collect()
}
