// Process resource-limit policy.
//
// TermAl supervises long-lived shared agent runtimes. Those children inherit
// this process's resource limits, so the Unix open-file soft limit must be
// raised before any runtime is spawned. macOS commonly launches applications
// with a soft RLIMIT_NOFILE of 256 even when the hard and kernel limits are
// much higher; one shared Codex app-server serving many sessions can exhaust
// that inherited ceiling and then fail to spawn ordinary commands.

#[cfg(any(unix, test))]
const DEFAULT_OPEN_FILE_SOFT_LIMIT: u64 = 8_192;
#[cfg(any(unix, test))]
const MAX_OPEN_FILE_SOFT_LIMIT: u64 = 1_048_576;
#[cfg(any(unix, test))]
const OPEN_FILE_LIMIT_ENV: &str = "TERMAL_NOFILE_LIMIT";

/// Raises the Unix process-wide open-file soft limit when headroom is available.
///
/// This is deliberately best-effort: an OS or policy that rejects the change
/// should not prevent TermAl from starting. All subsequently spawned agent
/// runtimes inherit the effective limit.
#[cfg(unix)]
fn configure_process_open_file_limit() {
    let requested = requested_open_file_soft_limit();
    configure_process_open_file_limit_with(
        requested,
        raise_process_open_file_soft_limit,
        |message| eprintln!("{message}"),
    );
}

#[cfg(any(unix, test))]
fn configure_process_open_file_limit_with(
    requested: u64,
    raise: impl FnOnce(u64) -> Result<OpenFileLimitChange, OpenFileLimitFailure>,
    mut report: impl FnMut(String),
) {
    match raise(requested) {
        Ok(change)
            if change.effective_soft > change.previous_soft
                || change.effective_soft < requested
                || change
                    .kernel_ceiling
                    .is_some_and(|ceiling| ceiling < requested) =>
        {
            let hard = change
                .hard_limit
                .map(|limit| limit.to_string())
                .unwrap_or_else(|| "unlimited".to_owned());
            let kernel = change
                .kernel_ceiling
                .map_or_else(|| "unknown".to_owned(), |n| n.to_string());
            let warning = if change.effective_soft < requested
                || change
                    .kernel_ceiling
                    .is_some_and(|ceiling| ceiling < requested)
            {
                "warning: "
            } else {
                ""
            };
            report(format!(
                "[termal] {warning}open-file soft limit effective {} \
                 (previous {}, requested {requested}, hard {hard}, kernel {kernel})",
                change.effective_soft, change.previous_soft
            ));
        }
        Ok(_) => {}
        Err(err) => {
            report(format!(
                "[termal] warning: open-file limit adjustment incomplete: {err}"
            ));
        }
    }
}

#[cfg(not(unix))]
fn configure_process_open_file_limit() {}

#[cfg(unix)]
fn requested_open_file_soft_limit() -> u64 {
    requested_open_file_soft_limit_with(std::env::var(OPEN_FILE_LIMIT_ENV), |message| {
        eprintln!("{message}")
    })
}

#[cfg(any(unix, test))]
fn requested_open_file_soft_limit_with(
    value: Result<String, std::env::VarError>,
    mut report: impl FnMut(String),
) -> u64 {
    match value {
        Ok(value) => match parse_open_file_soft_limit(&value) {
            Ok(limit) => {
                if value.parse::<u64>().is_ok_and(|raw| raw > limit) {
                    report(format!(
                        "[termal] warning: {OPEN_FILE_LIMIT_ENV} requested {value}; capped to safe maximum {MAX_OPEN_FILE_SOFT_LIMIT}"
                    ));
                }
                limit
            }
            Err(message) => {
                report(format!(
                    "[termal] warning: {message}; using default \
                     {DEFAULT_OPEN_FILE_SOFT_LIMIT}"
                ));
                DEFAULT_OPEN_FILE_SOFT_LIMIT
            }
        },
        Err(std::env::VarError::NotPresent) => DEFAULT_OPEN_FILE_SOFT_LIMIT,
        Err(err) => {
            report(format!(
                "[termal] warning: failed to read {OPEN_FILE_LIMIT_ENV}: {err}; \
                 using default {DEFAULT_OPEN_FILE_SOFT_LIMIT}"
            ));
            DEFAULT_OPEN_FILE_SOFT_LIMIT
        }
    }
}

#[cfg(any(unix, test))]
fn parse_open_file_soft_limit(value: &str) -> Result<u64, String> {
    let limit = value
        .parse::<u64>()
        .map_err(|_| format!("{OPEN_FILE_LIMIT_ENV} must be a positive integer, got `{value}`"))?;
    if limit == 0 {
        return Err(format!(
            "{OPEN_FILE_LIMIT_ENV} must be a positive integer, got `0`"
        ));
    }
    Ok(limit.min(MAX_OPEN_FILE_SOFT_LIMIT))
}

#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenFileLimitChange {
    previous_soft: u64,
    effective_soft: u64,
    hard_limit: Option<u64>,
    kernel_ceiling: Option<u64>,
}

#[cfg(any(unix, test))]
#[derive(Debug)]
struct OpenFileLimitFailure {
    requested: u64,
    limits: Option<(u64, Option<u64>)>,
    kernel_ceiling: Option<u64>,
    error: std::io::Error,
    readback_error: Option<std::io::Error>,
}

#[cfg(any(unix, test))]
impl std::fmt::Display for OpenFileLimitFailure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let effective = self
            .limits
            .map_or_else(|| "unknown".to_owned(), |(soft, _)| soft.to_string());
        let hard = match self.limits {
            None => "unknown".to_owned(),
            Some((_, None)) => "unlimited".to_owned(),
            Some((_, Some(hard))) => hard.to_string(),
        };
        let kernel = self
            .kernel_ceiling
            .map_or_else(|| "unknown".to_owned(), |n| n.to_string());
        write!(
            out,
            "requested {}, effective {effective}, hard {hard}, kernel {kernel}: {}",
            self.requested, self.error
        )?;
        if let Some(error) = &self.readback_error {
            write!(out, "; final limit readback failed: {error}")?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn read_process_open_file_limits() -> std::io::Result<libc::rlimit> {
    let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `limits` points to writable storage for one `rlimit`, and
    // getrlimit initializes it on success.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limits.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: getrlimit succeeded and initialized `limits`.
    Ok(unsafe { limits.assume_init() })
}

#[cfg(unix)]
fn process_open_file_limits_snapshot() -> std::io::Result<(u64, Option<u64>)> {
    let limits = read_process_open_file_limits()?;
    Ok((
        limits.rlim_cur as u64,
        (limits.rlim_max != libc::RLIM_INFINITY).then_some(limits.rlim_max as u64),
    ))
}

#[cfg(target_os = "macos")]
fn process_open_file_kernel_ceiling() -> Option<u64> {
    let mut ceiling: libc::c_int = 0;
    let mut size = std::mem::size_of_val(&ceiling);
    // SAFETY: the NUL-terminated name selects an integer sysctl; the output
    // buffer and its size are valid. A null new-value pointer makes this read-only.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            (&mut ceiling as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of_val(&ceiling) && ceiling > 0)
        .then_some(ceiling as u64)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn process_open_file_kernel_ceiling() -> Option<u64> {
    None
}

#[cfg(unix)]
fn raise_process_open_file_soft_limit(
    requested: u64,
) -> Result<OpenFileLimitChange, OpenFileLimitFailure> {
    let attempt = raise_open_file_soft_limit_with(
        requested,
        process_open_file_limits_snapshot,
        |effective_soft| {
            let mut limits = read_process_open_file_limits()?;
            // Preserve a concurrently raised soft limit and the current hard limit.
            if limits.rlim_cur as u64 >= effective_soft {
                return Ok(());
            }
            limits.rlim_cur = effective_soft as libc::rlim_t;
            // SAFETY: only the soft limit changes, capped to the finite hard limit.
            if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) } != 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        },
        process_open_file_kernel_ceiling(),
    );
    finalize_open_file_limit_change(requested, attempt, process_open_file_limits_snapshot)
}

#[cfg(any(unix, test))]
fn raise_open_file_soft_limit_with(
    requested: u64,
    read: impl FnOnce() -> std::io::Result<(u64, Option<u64>)>,
    mut write: impl FnMut(u64) -> std::io::Result<()>,
    kernel_ceiling: Option<u64>,
) -> Result<OpenFileLimitChange, OpenFileLimitFailure> {
    let failure = |limits, error| OpenFileLimitFailure {
        requested,
        limits,
        kernel_ceiling,
        error,
        readback_error: None,
    };
    let (previous_soft, hard_limit) = read().map_err(|error| failure(None, error))?;
    let capped = kernel_ceiling.map_or(requested, |ceiling| requested.min(ceiling));
    let mut effective_soft = desired_open_file_soft_limit(previous_soft, hard_limit, capped);
    if effective_soft > previous_soft {
        match write(effective_soft) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                // A hidden or changing kernel ceiling may reject an otherwise
                // valid soft-limit lift. Search only above the last success;
                // the policy cap bounds this to at most 20 additional calls.
                let mut low = previous_soft;
                let mut high = effective_soft - 1;
                while low < high {
                    let candidate = low + (high - low).div_ceil(2);
                    match write(candidate) {
                        Ok(()) => low = candidate,
                        Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => {
                            high = candidate - 1
                        }
                        Err(err) => return Err(failure(Some((low, hard_limit)), err)),
                    }
                }
                if low == previous_soft {
                    return Err(failure(Some((low, hard_limit)), error));
                }
                effective_soft = low;
            }
            Err(error) => return Err(failure(Some((previous_soft, hard_limit)), error)),
        }
    }
    Ok(OpenFileLimitChange {
        previous_soft,
        effective_soft,
        hard_limit,
        kernel_ceiling,
    })
}

#[cfg(any(unix, test))]
fn finalize_open_file_limit_change(
    requested: u64,
    attempt: Result<OpenFileLimitChange, OpenFileLimitFailure>,
    read: impl FnOnce() -> std::io::Result<(u64, Option<u64>)>,
) -> Result<OpenFileLimitChange, OpenFileLimitFailure> {
    // Even a rejected write (or a failed adapter read) may have observed a
    // newer limit than the policy's initial snapshot. Finalize both branches;
    // never replace the primary failure with a secondary readback failure.
    match (attempt, read()) {
        (Ok(mut change), Ok((effective, hard))) => {
            change.effective_soft = effective;
            change.hard_limit = hard;
            Ok(change)
        }
        (Ok(change), Err(error)) => Err(OpenFileLimitFailure {
            requested,
            limits: None,
            kernel_ceiling: change.kernel_ceiling,
            error,
            readback_error: None,
        }),
        (Err(mut failure), observed) => {
            match observed {
                Ok(limits) => failure.limits = Some(limits),
                Err(error) => {
                    failure.limits = None;
                    failure.readback_error = Some(error);
                }
            }
            Err(failure)
        }
    }
}

#[cfg(any(unix, test))]
fn desired_open_file_soft_limit(current_soft: u64, hard_limit: Option<u64>, requested: u64) -> u64 {
    let requested = requested.min(MAX_OPEN_FILE_SOFT_LIMIT);
    let requested = hard_limit.map_or(requested, |hard| requested.min(hard));
    current_soft.max(requested)
}

#[cfg(test)]
#[path = "process_limits_tests.rs"]
mod process_limit_tests;
