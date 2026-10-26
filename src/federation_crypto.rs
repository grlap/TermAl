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
    federation_control_write_frame(io, bytes)
        .await
        .map_err(|failure| failure.error)
}

// Keep framing and status mapping in this owner; callers receive fixed
// diagnostic classifications, never frame contents or raw OS error messages.
struct FederationControlFrameFailure {
    error: ApiError,
    reason: &'static str,
    io_kind: Option<&'static str>,
}

fn federation_control_io_kind(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::UnexpectedEof => "unexpectedEof",
        ErrorKind::ConnectionReset => "connectionReset",
        ErrorKind::ConnectionAborted => "connectionAborted",
        ErrorKind::ConnectionRefused => "connectionRefused",
        ErrorKind::HostUnreachable => "hostUnreachable",
        ErrorKind::NetworkUnreachable => "networkUnreachable",
        ErrorKind::NetworkDown => "networkDown",
        ErrorKind::AddrInUse => "addrInUse",
        ErrorKind::AddrNotAvailable => "addrNotAvailable",
        ErrorKind::BrokenPipe => "brokenPipe",
        ErrorKind::NotConnected => "notConnected",
        ErrorKind::TimedOut => "timedOut",
        ErrorKind::Interrupted => "interrupted",
        ErrorKind::WouldBlock => "wouldBlock",
        ErrorKind::PermissionDenied => "permissionDenied",
        ErrorKind::NotFound => "notFound",
        ErrorKind::AlreadyExists => "alreadyExists",
        ErrorKind::Unsupported => "unsupported",
        ErrorKind::InvalidData => "invalidData",
        ErrorKind::InvalidInput => "invalidInput",
        ErrorKind::WriteZero => "writeZero",
        ErrorKind::OutOfMemory => "outOfMemory",
        _ => "other",
    }
}

struct FederationControlRead<'a, T> {
    io: &'a mut T,
    bytes: usize,
    eof: bool,
    error: Option<std::io::ErrorKind>,
}

impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for FederationControlRead<'_, T> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let capacity = buffer.remaining();
        let result = std::pin::Pin::new(&mut *self.io).poll_read(cx, buffer);
        match &result {
            std::task::Poll::Ready(Ok(())) => {
                let count = buffer.filled().len() - before;
                self.bytes += count;
                self.eof |= capacity > 0 && count == 0;
            }
            std::task::Poll::Ready(Err(error)) => self.error = Some(error.kind()),
            std::task::Poll::Pending => {}
        }
        result
    }
}

async fn federation_control_read_frame(
    io: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<Vec<u8>, FederationControlFrameFailure> {
    let mut observed = FederationControlRead {
        io,
        bytes: 0,
        eof: false,
        error: None,
    };
    // Observe the existing reader rather than implementing another decoder.
    federation_read_frame(&mut observed).await.map_err(|error| {
        let eof = observed.eof || observed.error == Some(std::io::ErrorKind::UnexpectedEof);
        let reason = if eof {
            if observed.bytes == 0 {
                "controlEof"
            } else if observed.bytes < 4 {
                "controlEofPartialLength"
            } else {
                "controlEofPartialFrame"
            }
        } else if observed.error.is_some() {
            "controlReadIo"
        } else {
            "controlFrameLengthInvalid"
        };
        FederationControlFrameFailure {
            error,
            reason,
            io_kind: observed.error.map(federation_control_io_kind),
        }
    })
}

async fn federation_control_write_frame(
    io: &mut (impl tokio::io::AsyncWrite + Unpin),
    bytes: &[u8],
) -> Result<(), FederationControlFrameFailure> {
    use tokio::io::AsyncWriteExt;
    if bytes.is_empty() || bytes.len() > FEDERATION_FRAME_LIMIT {
        return Err(FederationControlFrameFailure {
            error: federation_auth_refused(),
            reason: "controlWriteFrameLengthInvalid",
            io_kind: None,
        });
    }
    let failure = |error: std::io::Error| FederationControlFrameFailure {
        error: federation_transport_unavailable(),
        reason: "controlWriteIo",
        io_kind: Some(federation_control_io_kind(error.kind())),
    };
    io.write_u32(bytes.len() as u32)
        .await
        .map_err(failure)?;
    io.write_all(bytes)
        .await
        .map_err(failure)?;
    io.flush()
        .await
        .map_err(failure)
}
