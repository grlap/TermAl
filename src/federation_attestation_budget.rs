// Owns separate SSH startup, peer-proof and local-install classifications.
// Does not change durability deadlines or heartbeat liveness. Split from federation_monitor.rs.

const FEDERATION_LINK_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
// The satellite durably installs before tag_s: allow its five-second fence
// plus two network round trips. The central's own install is a later stage.
const FEDERATION_ATTEST_TIMEOUT: Duration = Duration::from_secs(10);

fn federation_timeout_setting(value: Option<&str>, default: Duration) -> (Duration, bool) {
    match value {
        None => (default, false),
        Some(value) => match value.parse::<u64>() {
            Ok(ms) if (1_000..=120_000).contains(&ms) => (Duration::from_millis(ms), false),
            _ => (default, true),
        },
    }
}

fn federation_timeout_from_env(name: &str, default: Duration) -> Duration {
    let value = std::env::var_os(name);
    let (duration, invalid) = match value.as_ref() {
        None => (default, false),
        Some(value) => federation_timeout_setting(value.to_str(), default),
    };
    if invalid || value.as_ref().is_some_and(|value| value.to_str().is_none()) {
        eprintln!("invalid {name}; using federation timeout default");
    }
    duration
}

fn federation_attestation_error(
    channel: &FederationChannel,
    owner: Option<u64>,
    stage: &'static str,
    reason: &'static str,
    error: ApiError,
    crypto_refusal: bool,
) -> ApiError {
    channel.observe_attestation_failure(owner, stage, reason, crypto_refusal);
    error
}

async fn federation_attest_central_started(
    channel: &FederationChannel,
    io: &mut FederationControlIo,
    started: tokio::time::Instant,
) -> Result<(Arc<FederationSecurityContext>, FederationControlGuard), ApiError> {
    use tokio::io::AsyncWriteExt;
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
    let startup = async {
        io.write_all(FEDERATION_CONTROL_PREFACE)
            .await
            .map_err(|_| federation_transport_unavailable())?;
        // The full bounded frame, including its length prefix, belongs to
        // startup. A partial prefix/body cannot start or extend proof time.
        federation_read_frame(io).await
    };
    let hello = match tokio::time::timeout_at(
        started + channel.control.link_startup_timeout,
        startup,
    )
    .await
    {
        Ok(Ok(hello)) => hello,
        Ok(Err(error)) if error.status == StatusCode::CONFLICT => {
            return Err(federation_attestation_error(
                channel,
                Some(owner),
                "attest",
                "invalidFrame",
                error,
                true,
            ));
        }
        Ok(Err(error)) => {
            return Err(federation_attestation_error(
                channel,
                Some(owner),
                "startup",
                "linkDownBeforeHello",
                error,
                false,
            ));
        }
        Err(_) => {
            return Err(federation_attestation_error(
                channel,
                Some(owner),
                "startup",
                "startupExpired",
                federation_transport_unavailable(),
                false,
            ));
        }
    };
    let context = match tokio::time::timeout(
        channel.control.attest_timeout,
        federation_prove_central(channel, io, owner, hello),
    )
    .await
    {
        Ok(Ok(context)) => context,
        Ok(Err(error)) => {
            let crypto = error.status == StatusCode::CONFLICT;
            return Err(federation_attestation_error(
                channel,
                Some(owner),
                "attest",
                if crypto {
                    "proofRefused"
                } else {
                    "linkDownBeforeTag"
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
    // M1 owns the unchanged five-second durability fence and typed errors.
    // Do not wrap it in the proof timer or collapse its 500/423 into a 409.
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
    Ok((context, guard))
}
