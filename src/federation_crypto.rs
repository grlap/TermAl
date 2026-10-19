// Owns canonical bounded TLV framing, OS randomness and RustCrypto operations.
// Does not install contexts or authorize RPCs. New A1 protocol primitive leaf.

const FEDERATION_CONTROL_PREFACE: &[u8] = b"TERMAL-CTRL/1\n";
const FEDERATION_MAC_HEADER: &str = "x-termal-link-mac";
const FEDERATION_FRAME_LIMIT: usize = 65536;

fn federation_auth_refused() -> ApiError {
    ApiError::conflict("federation message authentication refused")
}

fn federation_random<const N: usize>() -> Result<[u8; N], ApiError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| federation_transport_unavailable())?;
    Ok(bytes)
}

fn federation_encode(domain: &[u8], values: &[&[u8]]) -> Vec<u8> {
    let mut bytes = vec![1];
    for (index, value) in std::iter::once(domain)
        .chain(values.iter().copied())
        .enumerate()
    {
        // Callers supply fixed field lists and bounded scalar/header values;
        // bodies are represented by their digest, never embedded here.
        bytes.push((index + 1) as u8);
        bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
        bytes.extend_from_slice(value);
    }
    bytes
}

fn federation_decode<'a>(
    bytes: &'a [u8],
    domain: &[u8],
    fields: usize,
) -> Result<Vec<&'a [u8]>, ApiError> {
    if bytes.first() != Some(&1) || bytes.len() > FEDERATION_FRAME_LIMIT || fields > 32 {
        return Err(federation_auth_refused());
    }
    let mut offset = 1_usize;
    let mut values = Vec::with_capacity(fields + 1);
    for tag in 1..=fields + 1 {
        let header = bytes
            .get(offset..offset + 5)
            .ok_or_else(federation_auth_refused)?;
        if usize::from(header[0]) != tag {
            return Err(federation_auth_refused());
        }
        let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        offset += 5;
        let end = offset
            .checked_add(length)
            .ok_or_else(federation_auth_refused)?;
        values.push(bytes.get(offset..end).ok_or_else(federation_auth_refused)?);
        offset = end;
    }
    if offset != bytes.len() || values.first().copied() != Some(domain) {
        return Err(federation_auth_refused());
    }
    values.remove(0);
    Ok(values)
}

fn federation_mac(key: &[u8], parts: &[&[u8]]) -> Result<Vec<u8>, ApiError> {
    use hmac::Mac;
    let mut mac =
        hmac::Hmac::<Sha256>::new_from_slice(key).map_err(|_| federation_auth_refused())?;
    for part in parts {
        mac.update(part);
    }
    Ok(mac.finalize().into_bytes().to_vec())
}

fn federation_verify_mac(key: &[u8], parts: &[&[u8]], tag: &[u8]) -> Result<(), ApiError> {
    use hmac::Mac;
    if tag.len() != 32 {
        return Err(federation_auth_refused());
    }
    let mut mac =
        hmac::Hmac::<Sha256>::new_from_slice(key).map_err(|_| federation_auth_refused())?;
    for part in parts {
        mac.update(part);
    }
    mac.verify_slice(tag).map_err(|_| federation_auth_refused())
}

impl FederationPairCapability {
    fn key(&self) -> Result<zeroize::Zeroizing<[u8; 32]>, ApiError> {
        let mut bytes = zeroize::Zeroizing::new([0; 32]);
        for (index, hex) in self.0.as_bytes().chunks_exact(2).enumerate() {
            let high = char::from(hex[0])
                .to_digit(16)
                .ok_or_else(federation_auth_refused)?;
            let low = char::from(hex[1])
                .to_digit(16)
                .ok_or_else(federation_auth_refused)?;
            bytes[index] = ((high << 4) | low) as u8;
        }
        Ok(bytes)
    }
}

async fn federation_read_frame(
    io: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<Vec<u8>, ApiError> {
    use tokio::io::AsyncReadExt;
    let length = io
        .read_u32()
        .await
        .map_err(|_| federation_transport_unavailable())? as usize;
    if length == 0 || length > FEDERATION_FRAME_LIMIT {
        return Err(federation_auth_refused());
    }
    let mut bytes = vec![0; length];
    io.read_exact(&mut bytes)
        .await
        .map_err(|_| federation_transport_unavailable())?;
    Ok(bytes)
}

async fn federation_write_frame(
    io: &mut (impl tokio::io::AsyncWrite + Unpin),
    bytes: &[u8],
) -> Result<(), ApiError> {
    use tokio::io::AsyncWriteExt;
    if bytes.is_empty() || bytes.len() > FEDERATION_FRAME_LIMIT {
        return Err(federation_auth_refused());
    }
    io.write_u32(bytes.len() as u32)
        .await
        .map_err(|_| federation_transport_unavailable())?;
    io.write_all(bytes)
        .await
        .map_err(|_| federation_transport_unavailable())?;
    io.flush()
        .await
        .map_err(|_| federation_transport_unavailable())
}
