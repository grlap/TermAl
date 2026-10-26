// Owns symmetric authenticated heartbeat deadlines and freshness sequencing.
// Does not carry commands or refresh liveness from RPCs. New A1 control FSM.

fn federation_control_sequence(bytes: &[u8]) -> Result<u64, ApiError> {
    let array: [u8; 8] = bytes.try_into().map_err(|_| federation_auth_refused())?;
    Ok(u64::from_be_bytes(array))
}

async fn federation_control_satellite(channel: FederationChannel, mut io: FederationControlIo) {
    let (context, _guard) = match federation_attest_satellite(&channel, &mut io).await {
        Ok(value) => value,
        Err(_) => return,
    };
    let settings = &channel.control;
    let mut terminal = FederationControlEnd {
        channel: &channel,
        context: &context,
        reason: "controlError",
        sequence: 0,
    };
    let mut deadline = settings.clock.now() + settings.timeout;
    let mut last_sequence = 0;
    let mut retired = context.retired.subscribe();
    loop {
        if *retired.borrow() {
            terminal.reason = "controlContextChanged";
            break;
        }
        let frame = tokio::select! {
            biased;
            _ = retired.changed() => { terminal.reason = "controlContextChanged"; break; },
            _ = settings.clock.deadline(deadline) => { terminal.reason = "controlExpired"; break; },
            result = federation_read_frame(&mut io) => match result { Ok(frame) => frame, Err(_) => break },
        };
        if channel.security_current(&context).is_err() {
            terminal.reason = "controlContextChanged";
            break;
        }
        let fields = match federation_decode(&frame, b"termal-hb-req-v1", 3) {
            Ok(fields) => fields,
            Err(_) => break,
        };
        let sequence = match federation_control_sequence(fields[0]) {
            Ok(value) => value,
            Err(_) => break,
        };
        let valid = fields[1].len() == 16
            && federation_verify_mac(
                &context.keys[4],
                &[b"hb-req", &context.transcript, fields[0], fields[1]],
                fields[2],
            )
            .is_ok();
        if !valid || sequence <= last_sequence {
            settings.observe("requestRefused", sequence);
            continue;
        }
        if settings.clock.now() >= deadline {
            terminal.reason = "controlExpired";
            break;
        }
        last_sequence = sequence;
        terminal.sequence = sequence;
        deadline = settings.clock.now() + settings.timeout;
        let tag = match federation_mac(
            &context.keys[5],
            &[b"hb-resp", &context.transcript, fields[0], fields[1]],
        ) {
            Ok(tag) => tag,
            Err(_) => break,
        };
        let response = federation_encode(b"termal-hb-resp-v1", &[fields[0], &tag]);
        let result = tokio::select! {
            biased;
            _ = settings.clock.deadline(deadline) => { terminal.reason = "controlExpired"; break; },
            result = federation_write_frame(&mut io, &response) => result,
        };
        if result.is_err() {
            break;
        }
        settings.observe("requestAccepted", sequence);
    }
    // The owned guard withdraws only this context/UUID, including EOF and
    // expiry. A delayed old task cannot fence a newer successful handshake.
}

async fn federation_control_central(
    channel: FederationChannel,
    io: FederationControlIo,
    context: Arc<FederationSecurityContext>,
    _guard: FederationControlGuard,
    mut stop: tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<()>,
) -> Result<(), ApiError> {
    let (mut reader, mut writer) = tokio::io::split(io);
    let settings = &channel.control;
    let mut terminal = FederationControlEnd {
        channel: &channel,
        context: &context,
        reason: "controlError",
        sequence: 0,
    };
    let mut deadline = settings.clock.now() + settings.timeout;
    let mut next_send = settings.clock.now();
    let mut sequence = 0_u64;
    let mut outstanding: Option<(u64, [u8; 16])> = None;
    let mut ready = Some(ready);
    let mut retired = context.retired.subscribe();
    loop {
        let frame = federation_read_frame(&mut reader);
        tokio::pin!(frame);
        // Retain a partially read frame across the send timer. Dropping and
        // restarting read_exact here would reopen an ambiguous frame boundary.
        let frame = loop {
            if *retired.borrow() || channel.security_current(&context).is_err() {
                terminal.reason = "controlContextChanged";
                return Err(federation_auth_refused());
            }
            tokio::select! {
                biased;
                _ = federation_wait_stop(&mut stop) => { terminal.reason = "controlStopped"; return Ok(()); },
                _ = retired.changed() => { terminal.reason = "controlContextChanged"; return Err(federation_auth_refused()); },
                _ = settings.clock.deadline(deadline) => { terminal.reason = "controlExpired"; return Err(federation_transport_unavailable()); },
                _ = settings.clock.deadline(next_send), if outstanding.is_none() => {
                    sequence = sequence.checked_add(1).ok_or_else(federation_auth_refused)?;
                    terminal.sequence = sequence;
                    let challenge = federation_random::<16>()?;
                    let number = sequence.to_be_bytes();
                    let tag = federation_mac(&context.keys[4], &[b"hb-req", &context.transcript, &number, &challenge])?;
                    let request = federation_encode(b"termal-hb-req-v1", &[&number, &challenge, &tag]);
                    tokio::select! {
                        biased;
                        _ = federation_wait_stop(&mut stop) => { terminal.reason = "controlStopped"; return Ok(()); },
                        _ = retired.changed() => { terminal.reason = "controlContextChanged"; return Err(federation_auth_refused()); },
                        _ = settings.clock.deadline(deadline) => { terminal.reason = "controlExpired"; return Err(federation_transport_unavailable()); },
                        result = federation_write_frame(&mut writer, &request) => result?,
                    }
                    outstanding = Some((sequence, challenge));
                },
                result = &mut frame => break result?,
            }
        };
        let fields = federation_decode(&frame, b"termal-hb-resp-v1", 2)?;
        let received = federation_control_sequence(fields[0])?;
        let valid = outstanding.as_ref().is_some_and(|(number, challenge)| {
            received == *number
                && federation_verify_mac(
                    &context.keys[5],
                    &[b"hb-resp", &context.transcript, fields[0], challenge],
                    fields[1],
                )
                .is_ok()
        });
        if !valid || settings.clock.now() >= deadline {
            settings.observe("replyRefused", received);
            continue;
        }
        channel.security_current(&context)?;
        outstanding = None;
        deadline = settings.clock.now() + settings.timeout;
        next_send = settings.clock.now() + settings.interval;
        if let Some(ready) = ready.take() {
            let _ = ready.send(());
        }
        settings.observe("replyAccepted", received);
    }
}
