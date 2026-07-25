// Process resource-limit policy.
//
// TermAl supervises long-lived shared agent runtimes. Those children inherit
// this process's resource limits, so the Unix open-file soft limit must be
// raised before any runtime is spawned. macOS commonly launches applications
// with a soft RLIMIT_NOFILE of 256 even when the hard and kernel limits are
// much higher; one shared Codex app-server serving many sessions can exhaust
// that inherited ceiling and then fail to spawn ordinary commands.

#[cfg(unix)]
const DEFAULT_OPEN_FILE_SOFT_LIMIT: u64 = 8_192;
#[cfg(unix)]
const OPEN_FILE_LIMIT_ENV: &str = "TERMAL_NOFILE_LIMIT";

/// Raises the Unix process-wide open-file soft limit when headroom is available.
///
/// This is deliberately best-effort: an OS or policy that rejects the change
/// should not prevent TermAl from starting. All subsequently spawned agent
/// runtimes inherit the effective limit.
#[cfg(unix)]
fn configure_process_open_file_limit() {
    let requested = requested_open_file_soft_limit();
    match raise_process_open_file_soft_limit(requested) {
        Ok(change) if change.effective_soft > change.previous_soft => {
            let hard = change
                .hard_limit
                .map(|limit| limit.to_string())
                .unwrap_or_else(|| "unlimited".to_owned());
            eprintln!(
                "[termal] raised open-file soft limit {} -> {} \
                 (requested {requested}, hard {hard})",
                change.previous_soft, change.effective_soft
            );
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!(
                "[termal] warning: failed to raise open-file soft limit \
                 toward {requested}: {err}"
            );
        }
    }
}

#[cfg(not(unix))]
fn configure_process_open_file_limit() {}

#[cfg(unix)]
fn requested_open_file_soft_limit() -> u64 {
    match std::env::var(OPEN_FILE_LIMIT_ENV) {
        Ok(value) => match parse_open_file_soft_limit(&value) {
            Ok(limit) => limit,
            Err(message) => {
                eprintln!(
                    "[termal] warning: {message}; using default \
                     {DEFAULT_OPEN_FILE_SOFT_LIMIT}"
                );
                DEFAULT_OPEN_FILE_SOFT_LIMIT
            }
        },
        Err(std::env::VarError::NotPresent) => DEFAULT_OPEN_FILE_SOFT_LIMIT,
        Err(err) => {
            eprintln!(
                "[termal] warning: failed to read {OPEN_FILE_LIMIT_ENV}: {err}; \
                 using default {DEFAULT_OPEN_FILE_SOFT_LIMIT}"
            );
            DEFAULT_OPEN_FILE_SOFT_LIMIT
        }
    }
}

#[cfg(unix)]
fn parse_open_file_soft_limit(value: &str) -> Result<u64, String> {
    let limit = value.parse::<u64>().map_err(|_| {
        format!("{OPEN_FILE_LIMIT_ENV} must be a positive integer, got `{value}`")
    })?;
    if limit == 0 {
        return Err(format!(
            "{OPEN_FILE_LIMIT_ENV} must be a positive integer, got `0`"
        ));
    }
    Ok(limit)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenFileLimitChange {
    previous_soft: u64,
    effective_soft: u64,
    hard_limit: Option<u64>,
}

#[cfg(unix)]
fn raise_process_open_file_soft_limit(
    requested: u64,
) -> std::io::Result<OpenFileLimitChange> {
    let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `limits` points to writable storage for one `rlimit`, and
    // getrlimit initializes it on success.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limits.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: getrlimit succeeded and initialized `limits`.
    let mut limits = unsafe { limits.assume_init() };

    let previous_soft = limits.rlim_cur as u64;
    let hard_limit = if limits.rlim_max == libc::RLIM_INFINITY {
        None
    } else {
        Some(limits.rlim_max as u64)
    };
    let effective_soft =
        desired_open_file_soft_limit(previous_soft, hard_limit, requested);

    if effective_soft > previous_soft {
        limits.rlim_cur = effective_soft as libc::rlim_t;
        // SAFETY: `limits` came from getrlimit; only the soft value changed,
        // and it was capped to the finite hard value when one exists.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }

    Ok(OpenFileLimitChange {
        previous_soft,
        effective_soft,
        hard_limit,
    })
}

#[cfg(unix)]
fn desired_open_file_soft_limit(
    current_soft: u64,
    hard_limit: Option<u64>,
    requested: u64,
) -> u64 {
    let requested = hard_limit.map_or(requested, |hard| requested.min(hard));
    current_soft.max(requested)
}

#[cfg(all(test, unix))]
mod process_limit_tests {
    use super::*;

    #[test]
    fn open_file_limit_target_raises_caps_and_never_lowers() {
        assert_eq!(desired_open_file_soft_limit(256, None, 8_192), 8_192);
        assert_eq!(
            desired_open_file_soft_limit(256, Some(4_096), 8_192),
            4_096
        );
        assert_eq!(
            desired_open_file_soft_limit(16_384, Some(32_768), 8_192),
            16_384
        );
    }

    #[test]
    fn open_file_limit_override_requires_a_positive_integer() {
        assert_eq!(parse_open_file_soft_limit("4096"), Ok(4_096));
        assert!(parse_open_file_soft_limit("0").is_err());
        assert!(parse_open_file_soft_limit("many").is_err());
    }

    #[test]
    fn process_open_file_limit_can_be_raised_without_lowering_it() {
        let before = raise_process_open_file_soft_limit(1)
            .expect("current open-file limit should be readable");
        let change = raise_process_open_file_soft_limit(DEFAULT_OPEN_FILE_SOFT_LIMIT)
            .expect("open-file soft limit should be configurable");
        assert!(change.effective_soft >= before.effective_soft);
        assert_eq!(
            change.effective_soft,
            desired_open_file_soft_limit(
                before.effective_soft,
                before.hard_limit,
                DEFAULT_OPEN_FILE_SOFT_LIMIT,
            )
        );
    }
}
