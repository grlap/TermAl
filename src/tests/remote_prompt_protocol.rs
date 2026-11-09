// Codec/reader controls only. No endpoint, transport, authentication or durable
// queue is exercised here; those integration tests remain required separately.
use super::*;
use std::io::{self, Cursor};

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn own(text: &str) -> OwnPrompt {
    OwnPrompt {
        reference: id(5),
        idempotency_key: "exact-key".into(),
        text: text.into(),
    }
}

fn intent(text: &str) -> Intent {
    Intent::new(
        AuthorityBasis {
            source: NativeRoot::new(id(1), 1, "session-11".into(), id(11)).unwrap(),
            target: NativeRoot::new(id(2), 1, "session-22".into(), id(22)).unwrap(),
            grant: DecisionIdentity::new(id(3), 1).unwrap(),
            consent: DecisionIdentity::new(id(4), 1).unwrap(),
            enrollment_generation: NonZeroU64::new(1).unwrap(),
            placement_generation: None,
        },
        id(5),
        "exact-key".into(),
        text.into(),
    )
    .unwrap()
}

fn proof() -> UnverifiedProof {
    UnverifiedProof::new(id(6), id(7), 100).unwrap()
}

fn remote(text: &str) -> Vec<u8> {
    encode_receive(&intent(text), &proof()).unwrap()
}

fn add_metadata_padding(mut bytes: Vec<u8>, current: usize, desired: usize) -> Vec<u8> {
    assert!(current <= desired);
    bytes.extend(std::iter::repeat_n(b' ', desired - current));
    bytes
}

#[test]
fn remote_prompt_codec_preserves_exact_text_and_body_free_debug() {
    let text = "sensitive canary\r\n\t𝄞\ne\u{301}\n";
    let local = own(text);
    let bytes = encode_own(&local).unwrap();
    assert_eq!(decode_own(&bytes).unwrap(), local);
    let encoded = remote(text);
    let decoded = decode_receive(&encoded).unwrap();
    assert_eq!(decoded.intent, intent(text));
    assert_eq!(decoded.retained_wire_bytes, encoded.len());
    assert_eq!(decoded.proof, proof());
    assert!(!format!("{local:?}").contains("sensitive canary"));
    assert!(!format!("{decoded:?}").contains("sensitive canary"));
}

#[test]
fn remote_prompt_codec_decoded_text_exact_65536_and_65537() {
    for text in [
        "a".repeat(TEXT_BYTES),
        "é".repeat(TEXT_BYTES / 2),
        "𝄞".repeat(TEXT_BYTES / 4),
    ] {
        assert_eq!(text.len(), TEXT_BYTES);
        assert_eq!(
            decode_own(&encode_own(&own(&text)).unwrap()).unwrap().text,
            text
        );
        assert_eq!(
            decode_receive(&remote(&text)).unwrap().intent,
            intent(&text)
        );
        let oversized = format!("{text}a");
        assert_eq!(encode_own(&own(&oversized)), Err(DecodeError::TextTooLarge));
        assert_eq!(
            encode_receive(&intent(&oversized), &proof()),
            Err(DecodeError::TextTooLarge)
        );
        // Independently exercise ingress, not only local serialization checks.
        let raw = format!(
            "{{\"targetReference\":\"prompt-reference-{}\",\"idempotencyKey\":\"key\",\"text\":{}}}",
            id(5),
            serde_json::to_string(&oversized).unwrap()
        );
        assert_eq!(decode_own(raw.as_bytes()), Err(DecodeError::TextTooLarge));
    }
}

#[test]
fn remote_prompt_codec_worst_case_escaping_fits_without_enlarging_other_limits() {
    let text = "\0".repeat(TEXT_BYTES);
    let bytes = remote(&text);
    let envelope: ReceiveRequest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(envelope.text.get().len(), 6 * TEXT_BYTES + 2);
    assert!(bytes.len() <= REQUEST_BYTES);
    assert!(bytes.len() - envelope.text.get().len() <= METADATA_BYTES);
    assert_eq!(decode_receive(&bytes).unwrap().intent, intent(&text));
}

#[test]
fn remote_prompt_codec_metadata_counts_quotes_separators_and_whitespace_exactly() {
    let base = encode_own(&own("abc")).unwrap();
    let request: OwnRequest = serde_json::from_slice(&base).unwrap();
    let metadata = base.len() - request.text.get().len();
    assert_eq!(request.text.get(), "\"abc\"");
    let exact = add_metadata_padding(base, metadata, METADATA_BYTES);
    assert_eq!(decode_own(&exact).unwrap(), own("abc"));
    let mut too_much = exact;
    too_much.push(b' ');
    assert_eq!(decode_own(&too_much), Err(DecodeError::MetadataTooLarge));
    let base = remote("abc");
    let request: ReceiveRequest = serde_json::from_slice(&base).unwrap();
    let metadata = base.len() - request.text.get().len();
    let exact = add_metadata_padding(base, metadata, METADATA_BYTES);
    assert_eq!(decode_receive(&exact).unwrap().intent, intent("abc"));
    let mut too_much = exact;
    too_much.push(b' ');
    assert_eq!(
        decode_receive(&too_much).unwrap_err(),
        DecodeError::MetadataTooLarge
    );
}

#[test]
fn remote_prompt_codec_equivalent_json_escaping_has_same_logical_digest() {
    let encoded = String::from_utf8(remote("A\r\n")).unwrap();
    let escaped = encoded.replace("\"text\":\"A\\r\\n\"", "\"text\":\"\\u0041\\u000d\\u000a\"");
    assert_ne!(encoded, escaped);
    let one = decode_receive(encoded.as_bytes()).unwrap();
    let two = decode_receive(escaped.as_bytes()).unwrap();
    assert_eq!(one.intent, two.intent);
    assert_eq!(one.intent.digest(), two.intent.digest());
    // MAC input is still different; the codec never substitutes canonical bytes.
    assert_ne!(encoded.as_bytes(), escaped.as_bytes());
}

#[test]
fn remote_prompt_codec_rejects_duplicate_unknown_and_claimed_source_on_own_route() {
    let base = String::from_utf8(encode_own(&own("abc")).unwrap()).unwrap();
    for field in [
        "\"text\":\"other\"",
        "\"\\u0074ext\":\"other\"",
        "\"source\":{}",
        "\"unknown\":null",
    ] {
        let changed = format!("{{{field},{}", &base[1..]);
        assert_eq!(
            decode_own(changed.as_bytes()),
            Err(DecodeError::InvalidJson)
        );
    }
    let encoded = String::from_utf8(remote("abc")).unwrap();
    for (from, to) in [
        (
            "\"protocolVersion\":1",
            "\"protocolVersion\":1,\"protocolVersion\":1",
        ),
        ("\"text\":\"abc\"", "\"text\":\"abc\",\"text\":\"abc\""),
        ("\"hostEpoch\":1", "\"hostEpoch\":1,\"hostEpoch\":1"),
        ("\"generation\":1", "\"generation\":1,\"unknown\":true"),
        (
            "\"expiresAt\":100",
            "\"expiresAt\":100,\"authenticated\":true",
        ),
    ] {
        let changed = encoded.replace(from, to);
        assert_ne!(changed, encoded);
        assert_eq!(
            decode_receive(changed.as_bytes()).unwrap_err(),
            DecodeError::InvalidJson
        );
    }
}

#[test]
fn remote_prompt_codec_rejects_nonstring_invalid_unicode_and_invalid_utf8() {
    let encoded = String::from_utf8(remote("abc")).unwrap();
    for token in [
        "null",
        "42",
        "{}",
        "[]",
        "\"\\ud800\"",
        "\"\\udfff\"",
        "\"\\ud800A\"",
    ] {
        let changed = encoded.replace("\"text\":\"abc\"", &format!("\"text\":{token}"));
        assert_eq!(
            decode_receive(changed.as_bytes()).unwrap_err(),
            DecodeError::InvalidJson
        );
    }
    let mut invalid = remote("abc");
    let offset = invalid
        .windows(3)
        .position(|window| window == b"abc")
        .unwrap();
    invalid[offset] = 0xff;
    assert_eq!(
        decode_receive(&invalid).unwrap_err(),
        DecodeError::InvalidJson
    );
    let paired = encoded.replace("\"abc\"", "\"\\ud834\\udd1e\"");
    assert_eq!(
        decode_receive(paired.as_bytes()).unwrap().intent,
        intent("𝄞")
    );
}

#[test]
fn remote_prompt_codec_strict_identity_version_and_mandatory_null_placement() {
    let encoded = String::from_utf8(remote("abc")).unwrap();
    for (from, to, error) in [
        (
            "\"protocolVersion\":1",
            "\"protocolVersion\":2",
            DecodeError::UnsupportedVersion,
        ),
        (
            "\"hostEpoch\":1",
            "\"hostEpoch\":0",
            DecodeError::InvalidIdentity,
        ),
        (
            "\"generation\":1",
            "\"generation\":0",
            DecodeError::InvalidIdentity,
        ),
        (
            "\"enrollmentGeneration\":1",
            "\"enrollmentGeneration\":0",
            DecodeError::InvalidIdentity,
        ),
        (
            "\"placementGeneration\":null",
            "\"placementGeneration\":0",
            DecodeError::InvalidIdentity,
        ),
        (
            "\"placementGeneration\":null,",
            "",
            DecodeError::InvalidJson,
        ),
        (
            "\"expiresAt\":100",
            "\"expiresAt\":0",
            DecodeError::InvalidIdentity,
        ),
        (
            "\"referenceId\":\"00000000-0000-0000-0000-000000000005\"",
            "\"referenceId\":\"00000000000000000000000000000005\"",
            DecodeError::InvalidIdentity,
        ),
        (
            "\"nonce\":\"00000000-0000-0000-0000-000000000007\"",
            "\"nonce\":\"00000000-0000-0000-0000-000000000000\"",
            DecodeError::InvalidIdentity,
        ),
    ] {
        let changed = encoded.replace(from, to);
        assert_ne!(changed, encoded);
        assert_eq!(decode_receive(changed.as_bytes()).unwrap_err(), error);
    }
}

struct Chunks<'a> {
    bytes: &'a [u8],
    total: usize,
    chunk: usize,
}

impl Read for Chunks<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.chunk.min(buffer.len()).min(self.bytes.len());
        buffer[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        self.total += count;
        Ok(count)
    }
}

#[test]
fn remote_prompt_codec_request_collector_stops_at_524289_even_when_chunked() {
    let bytes = vec![b' '; REQUEST_BYTES + 100];
    let mut reader = Chunks {
        bytes: &bytes,
        total: 0,
        chunk: 7,
    };
    assert_eq!(read_request(&mut reader), Err(DecodeError::RequestTooLarge));
    assert_eq!(reader.total, REQUEST_BYTES + 1);
    assert_eq!(
        read_request(Cursor::new(&bytes[..REQUEST_BYTES]))
            .unwrap()
            .len(),
        REQUEST_BYTES
    );
    assert_eq!(
        decode_receive(&bytes[..REQUEST_BYTES]).unwrap_err(),
        DecodeError::InvalidJson
    );
    assert_eq!(
        decode_receive(&bytes[..REQUEST_BYTES + 1]).unwrap_err(),
        DecodeError::RequestTooLarge
    );
}

#[test]
fn remote_prompt_codec_response_collector_stops_at_16385_even_when_chunked() {
    let bytes = vec![b'x'; RESPONSE_BYTES + 100];
    let mut reader = Chunks {
        bytes: &bytes,
        total: 0,
        chunk: 11,
    };
    assert_eq!(
        read_response(&mut reader),
        Err(DecodeError::ResponseTooLarge)
    );
    assert_eq!(reader.total, RESPONSE_BYTES + 1);
    assert_eq!(
        read_response(Cursor::new(&bytes[..RESPONSE_BYTES]))
            .unwrap()
            .len(),
        RESPONSE_BYTES
    );
}

#[test]
fn remote_prompt_codec_stdin_is_bounded_strict_utf8_and_keeps_newlines() {
    let text = "full body\r\nfinal newline\n";
    assert_eq!(read_stdin(Cursor::new(text.as_bytes())).unwrap(), text);
    assert_eq!(
        read_stdin(Cursor::new([0xc3, 0x28])),
        Err(DecodeError::InvalidUtf8)
    );
    let bytes = vec![b'a'; TEXT_BYTES + 100];
    let mut reader = Chunks {
        bytes: &bytes,
        total: 0,
        chunk: 5,
    };
    assert_eq!(read_stdin(&mut reader), Err(DecodeError::TextTooLarge));
    assert_eq!(reader.total, TEXT_BYTES + 1);
    assert_eq!(
        read_stdin(Cursor::new(&bytes[..TEXT_BYTES])).unwrap().len(),
        TEXT_BYTES
    );
    let mut split_utf8 = vec![b'a'; TEXT_BYTES - 1];
    split_utf8.push(0xc3);
    assert_eq!(
        read_stdin(Cursor::new(split_utf8)),
        Err(DecodeError::InvalidUtf8)
    );
}

struct Fails;
impl Read for Fails {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("sensitive body must not be echoed"))
    }
}

#[test]
fn remote_prompt_codec_io_and_truncated_body_fail_without_echo() {
    assert_eq!(read_request(Fails), Err(DecodeError::InputIo));
    assert_eq!(read_response(Fails), Err(DecodeError::InputIo));
    assert_eq!(read_stdin(Fails), Err(DecodeError::InputIo));
    let mut truncated = remote("abc");
    truncated.pop();
    assert_eq!(
        decode_receive(&truncated).unwrap_err(),
        DecodeError::InvalidJson
    );
    assert!(!format!("{:?}", read_response(Fails).unwrap_err()).contains("sensitive"));
}

#[test]
fn remote_prompt_codec_new_transport_proof_does_not_change_intent_digest() {
    let intent = intent("body");
    let one = decode_receive(&encode_receive(&intent, &proof()).unwrap()).unwrap();
    let new_proof = UnverifiedProof::new(id(66), id(77), 200).unwrap();
    let two = decode_receive(&encode_receive(&intent, &new_proof).unwrap()).unwrap();
    assert_eq!(one.intent.digest(), two.intent.digest());
    assert_ne!(one.proof, two.proof);
    assert_eq!(two.proof.components().unwrap(), (id(66), id(77), 200));
}
