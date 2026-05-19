/*
Telegram send-message helpers shared by digest and assistant forwarding paths.
*/

/// Telegram's `sendMessage` rejects bodies over 4096 UTF-16 code
/// units. Stay below that limit using the same unit Telegram counts.
const TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS: usize = 3500;

/// Splits `text` into chunks no longer than
/// `TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS`, preferring to break at the
/// last newline within each chunk window so chunks read like
/// natural prose paragraphs rather than mid-sentence cuts.
/// Falls back to a hard UTF-16-unit split when a single
/// line exceeds the limit (e.g., a giant URL or one-line code
/// dump).
fn chunk_telegram_message_text(text: &str) -> Vec<String> {
    if text.encode_utf16().count() <= TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS {
        return vec![text.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = start;
        let mut units = 0;
        let mut last_newline_end = None;
        for (offset, ch) in text[start..].char_indices() {
            let char_start = start + offset;
            let char_end = char_start + ch.len_utf8();
            let char_units = ch.len_utf16();
            if units + char_units > TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS {
                break;
            }

            units += char_units;
            end = char_end;
            if ch == '\n' {
                last_newline_end = Some(char_end);
            }
        }

        if end == start {
            let ch = text[start..]
                .chars()
                .next()
                .expect("chunk start should point at a character");
            end = start + ch.len_utf8();
        }

        let break_at = if end < text.len() {
            last_newline_end
                .filter(|&candidate| candidate > start)
                .unwrap_or(end)
        } else {
            end
        };
        let chunk = &text[start..break_at];
        if !chunk.is_empty() {
            chunks.push(chunk.to_owned());
        }
        start = break_at;
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}
