// Owns peer proof transcripts and mutual key confirmation on owned streams.
// Does not authenticate HTTP or run SSH. New A1 handshake leaf.

use super::*;

fn federation_transcript(
    channel: &FederationChannel,
    satellite_boot: &[u8],
    incarnation: &Uuid,
) -> Vec<u8> {
    let snapshot = &channel.snapshot;
    federation_encode(
        b"termal-transcript-v1",
        &[
            snapshot.local.host_id.as_bytes(),
            snapshot.peer.host_id.as_bytes(),
            &snapshot.enrollment_generation.to_be_bytes(),
            &snapshot.central.host_generation.to_be_bytes(),
            channel.server_instance_id.as_bytes(),
            satellite_boot,
            incarnation.as_bytes(),
            format!("127.0.0.1:{}", snapshot.endpoint.satellite_federation_port).as_bytes(),
        ],
    )
}

#[cfg(test)]
pub(crate) async fn federation_attest_central(
    channel: &FederationChannel,
    io: &mut FederationControlIo,
) -> Result<(Arc<FederationSecurityContext>, FederationControlGuard), ApiError> {
    let started = if channel.control.clock.io_deadlines {
        tokio::time::Instant::from_std(channel.control.clock.now())
    } else {
        tokio::time::Instant::now()
    };
    federation_attest_central_started(channel, io, started).await
}

pub(crate) async fn federation_prove_central(
    channel: &FederationChannel,
    io: &mut FederationControlIo,
    owner: u64,
    hello: Vec<u8>,
) -> Result<Arc<FederationSecurityContext>, ApiError> {
    let hello = federation_decode(&hello, b"termal-hello-s-v1", 4)?;
    let boot = std::str::from_utf8(hello[2]).map_err(|_| federation_auth_refused())?;
    if hello[0] != channel.snapshot.peer.host_id.as_bytes()
        || hello[1] != channel.snapshot.enrollment_generation.to_be_bytes()
        || Uuid::parse_str(boot).is_err()
        || hello[3].len() != 32
    {
        return Err(federation_auth_refused());
    }
    let incarnation = Uuid::new_v4();
    let transcript = federation_transcript(channel, hello[2], &incarnation);
    let nc = federation_random::<32>()?;
    let pair = channel.capability.key()?;
    let tag = federation_mac(
        pair.as_ref(),
        &[b"termal-link-attest-c2s-v1", &transcript, hello[3], &nc],
    )?;
    federation_write_frame(
        io,
        &federation_encode(b"termal-hello-c-v1", &[&transcript, &nc, &tag]),
    )
    .await?;
    let proof = federation_read_frame(io).await?;
    let proof = federation_decode(&proof, b"termal-hello-proof-v1", 1)?;
    federation_verify_mac(
        pair.as_ref(),
        &[b"termal-link-attest-s2c-v1", &transcript, &nc, hello[3]],
        proof[0],
    )?;
    let context = Arc::new(FederationSecurityContext::derive(
        owner,
        incarnation.to_string(),
        transcript,
        pair.as_ref(),
        hello[3],
        &nc,
    )?);
    Ok(context)
}

struct PendingSatelliteProof<'a> {
    channel: &'a FederationChannel,
    context: &'a FederationSecurityContext,
    guard: Option<FederationControlGuard>,
}

impl Drop for PendingSatelliteProof<'_> {
    fn drop(&mut self) {
        if !std::thread::panicking()
            && let Some(guard) = &self.guard
        {
            self.channel.record_terminal_event(
                self.context,
                "attest",
                "tagWriteCancelled",
                None,
                |state| guard.fence_locked(state),
            );
        }
    }
}

pub(crate) async fn federation_attest_satellite(
    channel: &FederationChannel,
    io: &mut FederationControlIo,
) -> Result<(Arc<FederationSecurityContext>, FederationControlGuard), ApiError> {
    let owner = channel.next_security_owner().map_err(|error| {
        federation_attestation_error(
            channel,
            None,
            "install",
            "authorityUnavailable",
            error,
            false,
        )
    })?;
    let (context, tag) = match channel
        .control
        .clock
        .timeout(
            channel.control.attest_timeout,
            federation_prove_satellite(channel, io, owner),
        )
        .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            let crypto = error.status == StatusCode::CONFLICT;
            return Err(federation_attestation_error(
                channel,
                Some(owner),
                "attest",
                if crypto {
                    "proofRefused"
                } else {
                    "linkDownBeforeProof"
                },
                error,
                crypto,
            ));
        }
        Err(_) => {
            return Err(federation_attestation_error(
                channel,
                Some(owner),
                "attest",
                "attestExpired",
                federation_auth_refused(),
                true,
            ));
        }
    };
    // The satellite publishes its durable UUID before readiness reaches C.
    // Its local M1 failure retains its typed status and is never a bad-key count.
    let guard = channel
        .install_security(context.clone())
        .await
        .map_err(|error| {
            federation_attestation_error(
                channel,
                Some(owner),
                "install",
                "localInstallFailed",
                error,
                false,
            )
        })?;
    let mut pending = PendingSatelliteProof {
        channel,
        context: &context,
        guard: Some(guard),
    };
    let proof = channel
        .control
        .clock
        .timeout(
            channel.control.attest_timeout,
            federation_control_write_frame(
                io,
                &federation_encode(b"termal-hello-proof-v1", &[&tag]),
            ),
        )
        .await;
    let guard = pending
        .guard
        .take()
        .expect("installed satellite proof guard");
    drop(pending);
    let (reason, io_kind, error) = match proof {
        Ok(Ok(())) => return Ok((context, guard)),
        Ok(Err(failure)) => ("linkDownBeforeTag", failure.io_kind, failure.error),
        Err(_) => ("tagWriteExpired", None, federation_transport_unavailable()),
    };
    channel.fence_terminal_event(&context, "attest", reason, io_kind, None, |state| {
        guard.fence_locked(state);
    });
    Err(federation_attestation_error(
        channel,
        Some(owner),
        "attest",
        reason,
        error,
        false,
    ))
}

async fn federation_prove_satellite(
    channel: &FederationChannel,
    io: &mut FederationControlIo,
    owner: u64,
) -> Result<(Arc<FederationSecurityContext>, Vec<u8>), ApiError> {
    let ns = federation_random::<32>()?;
    federation_write_frame(
        io,
        &federation_encode(
            b"termal-hello-s-v1",
            &[
                channel.snapshot.local.host_id.as_bytes(),
                &channel.snapshot.enrollment_generation.to_be_bytes(),
                channel.server_instance_id.as_bytes(),
                &ns,
            ],
        ),
    )
    .await?;
    let hello = federation_read_frame(io).await?;
    let hello = federation_decode(&hello, b"termal-hello-c-v1", 3)?;
    let fields = federation_decode(hello[0], b"termal-transcript-v1", 8)?;
    let central_boot = std::str::from_utf8(fields[4]).map_err(|_| federation_auth_refused())?;
    let incarnation = Uuid::from_slice(fields[6]).map_err(|_| federation_auth_refused())?;
    if fields[0] != channel.snapshot.peer.host_id.as_bytes()
        || fields[1] != channel.snapshot.local.host_id.as_bytes()
        || fields[2] != channel.snapshot.enrollment_generation.to_be_bytes()
        || fields[3] != channel.snapshot.central.host_generation.to_be_bytes()
        || Uuid::parse_str(central_boot).is_err()
        || fields[5] != channel.server_instance_id.as_bytes()
        || incarnation.is_nil()
        || fields[7]
            != format!(
                "127.0.0.1:{}",
                channel.snapshot.endpoint.satellite_federation_port
            )
            .as_bytes()
        || hello[1].len() != 32
    {
        return Err(federation_auth_refused());
    }
    let pair = channel.capability.key()?;
    federation_verify_mac(
        pair.as_ref(),
        &[b"termal-link-attest-c2s-v1", hello[0], &ns, hello[1]],
        hello[2],
    )?;
    let tag = federation_mac(
        pair.as_ref(),
        &[b"termal-link-attest-s2c-v1", hello[0], hello[1], &ns],
    )?;
    let context = Arc::new(FederationSecurityContext::derive(
        owner,
        incarnation.to_string(),
        hello[0].to_vec(),
        pair.as_ref(),
        &ns,
        hello[1],
    )?);
    Ok((context, tag))
}
