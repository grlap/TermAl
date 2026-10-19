// Owns independent wire encoding and RustCrypto operations for a test peer.
// Does not call the product encoder, MAC verifier or replay window. New protocol reference.

use super::*;
use hmac::Mac;

pub(super) const CONTROL_PREFACE: &[u8] = b"TERMAL-CTRL/1\n";
pub(super) const AUTH_HEADER: &str = "x-termal-link-mac";

pub(super) fn encode(domain: &[u8], values: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![1];
    for (index, value) in std::iter::once(domain)
        .chain(values.iter().map(Vec::as_slice))
        .enumerate()
    {
        bytes.push(u8::try_from(index + 1).unwrap());
        bytes.extend_from_slice(&u32::try_from(value.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(value);
    }
    bytes
}

pub(super) fn decode(bytes: &[u8], domain: &[u8], fields: usize) -> Result<Vec<Vec<u8>>, String> {
    if bytes.first() != Some(&1) || bytes.len() > 65536 {
        return Err("invalid protocol frame".into());
    }
    let mut offset = 1;
    let mut values = Vec::new();
    for tag in 1..=fields + 1 {
        let header = bytes.get(offset..offset + 5).ok_or("short field header")?;
        if header[0] != tag as u8 {
            return Err("non-canonical field order".into());
        }
        let length = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        offset += 5;
        let value = bytes
            .get(offset..offset.checked_add(length).ok_or("field overflow")?)
            .ok_or("short field value")?
            .to_vec();
        offset += length;
        values.push(value);
    }
    if offset != bytes.len() || values.remove(0) != domain {
        return Err("wrong domain or trailing bytes".into());
    }
    Ok(values)
}

pub(super) fn mac(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

pub(super) fn verify(key: &[u8], message: &[u8], tag: &[u8]) -> Result<(), String> {
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(message);
    mac.verify_slice(tag)
        .map_err(|_| "peer proof did not verify".into())
}

pub(super) fn pair_key(secret: char) -> [u8; 32] {
    let value = secret.to_digit(16).unwrap() as u8;
    [value * 17; 32]
}

pub(super) fn digest(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

pub(super) async fn write_frame(io: &mut tokio::net::TcpStream, body: &[u8]) -> Result<(), String> {
    io.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .map_err(|_| "control write failed")?;
    io.write_all(body)
        .await
        .map_err(|_| "control write failed")
        .map(|_| ())
        .map_err(str::to_owned)
}

pub(super) async fn read_frame(io: &mut tokio::net::TcpStream) -> Result<Vec<u8>, String> {
    let mut header = [0; 4];
    io.read_exact(&mut header)
        .await
        .map_err(|_| "control closed")?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > 65536 {
        return Err("control frame exceeds bound".into());
    }
    let mut body = vec![0; length];
    io.read_exact(&mut body)
        .await
        .map_err(|_| "control closed")?;
    Ok(body)
}

pub(super) fn joined(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[derive(Clone)]
pub(super) struct ReferenceSession {
    pub transcript: Vec<u8>,
    pub incarnation: String,
    pub keys: [[u8; 32]; 6],
}

impl ReferenceSession {
    pub fn derive(
        transcript: Vec<u8>,
        incarnation: String,
        ns: &[u8],
        nc: &[u8],
        secret: char,
    ) -> Self {
        let salt = joined(&[ns, nc]);
        let key = pair_key(secret);
        let hk = hkdf::Hkdf::<Sha256>::new(Some(&salt), &key);
        let mut keys = [[0; 32]; 6];
        for (key, label) in keys.iter_mut().zip([
            "termal-link-req-c2s-v1",
            "termal-link-resp-c2s-v1",
            "termal-link-req-s2c-v1",
            "termal-link-resp-s2c-v1",
            "termal-link-ctl-c2s-v1",
            "termal-link-ctl-s2c-v1",
        ]) {
            hk.expand(&joined(&[label.as_bytes(), &transcript]), key)
                .unwrap();
        }
        Self {
            transcript,
            incarnation,
            keys,
        }
    }

    pub fn heartbeat(&self, sequence: u64, challenge: &[u8]) -> Vec<u8> {
        let sequence = sequence.to_be_bytes();
        let tag = mac(
            &self.keys[4],
            &joined(&[b"hb-req", &self.transcript, &sequence, challenge]),
        );
        encode(
            b"termal-hb-req-v1",
            &[sequence.to_vec(), challenge.to_vec(), tag],
        )
    }

    pub fn check_heartbeat(
        &self,
        bytes: &[u8],
        sequence: u64,
        challenge: &[u8],
    ) -> Result<(), String> {
        let fields = decode(bytes, b"termal-hb-resp-v1", 2)?;
        if fields[0] != sequence.to_be_bytes() {
            return Err("wrong heartbeat sequence".into());
        }
        verify(
            &self.keys[5],
            &joined(&[
                b"hb-resp",
                &self.transcript,
                &sequence.to_be_bytes(),
                challenge,
            ]),
            &fields[1],
        )
    }
}
