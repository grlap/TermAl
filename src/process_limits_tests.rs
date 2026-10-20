use super::*;

fn selected(value: Result<String, std::env::VarError>) -> (u64, Vec<String>) {
    let mut notices = Vec::new();
    let limit = requested_open_file_soft_limit_with(value, |line| notices.push(line));
    (limit, notices)
}

#[test]
fn nofile_default_without_override_is_quiet() {
    assert_eq!(
        selected(Err(std::env::VarError::NotPresent)),
        (8_192, vec![])
    );
}

#[test]
fn nofile_valid_override_is_used_without_process_environment_mutation() {
    assert_eq!(selected(Ok("4096".into())), (4_096, vec![]));
}

#[test]
fn nofile_invalid_overrides_use_default_with_diagnostic() {
    for value in ["", "0", "-1", "many", "18446744073709551616"] {
        let (limit, notices) = selected(Ok(value.into()));
        assert_eq!(limit, DEFAULT_OPEN_FILE_SOFT_LIMIT, "{value:?}");
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains(OPEN_FILE_LIMIT_ENV));
        assert!(notices[0].contains("default"));
    }
}

#[test]
fn nofile_unreadable_override_uses_default_with_diagnostic() {
    let (limit, notices) = selected(Err(std::env::VarError::NotUnicode("opaque".into())));
    assert_eq!(limit, DEFAULT_OPEN_FILE_SOFT_LIMIT);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("failed to read TERMAL_NOFILE_LIMIT"));
}

#[test]
fn nofile_pathological_override_is_capped_to_safe_maximum() {
    // A policy ceiling, not a promise that this host permits that many files.
    assert_eq!(
        parse_open_file_soft_limit("18446744073709551615"),
        Ok(1_048_576)
    );
}

#[test]
fn nofile_override_selection_applies_the_same_cap() {
    assert_eq!(selected(Ok("1048577".into())).0, 1_048_576);
}

#[test]
fn nofile_target_raises_caps_and_never_lowers() {
    assert_eq!(desired_open_file_soft_limit(256, None, 8_192), 8_192);
    assert_eq!(desired_open_file_soft_limit(256, Some(4_096), 8_192), 4_096);
    assert_eq!(
        desired_open_file_soft_limit(16_384, Some(32_768), 8_192),
        16_384
    );
}

#[test]
fn nofile_successful_write_records_effective_limit() {
    let mut writes = Vec::new();
    let change = raise_open_file_soft_limit_with(
        8_192,
        || Ok((256, Some(16_384))),
        |target| {
            writes.push(target);
            Ok(())
        },
        None,
    )
    .unwrap();
    assert_eq!(writes, [8_192]);
    assert_eq!(
        change,
        OpenFileLimitChange {
            previous_soft: 256,
            effective_soft: 8_192,
            hard_limit: Some(16_384),
            kernel_ceiling: None,
        }
    );
}

#[test]
fn nofile_existing_higher_limit_is_not_written_even_above_policy_cap() {
    let change = raise_open_file_soft_limit_with(
        8_192,
        || Ok((2_000_000, None)),
        |_| panic!("must not lower an inherited limit"),
        Some(4_096),
    )
    .unwrap();
    assert_eq!(change.effective_soft, 2_000_000);
}

#[test]
fn nofile_mac_kernel_ceiling_lifts_instead_of_rejecting_infinite_hard_limit() {
    let mut writes = Vec::new();
    let change = raise_open_file_soft_limit_with(
        8_192,
        || Ok((256, None)),
        |target| {
            writes.push(target);
            if target > 4_096 {
                Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
            } else {
                Ok(())
            }
        },
        Some(4_096),
    )
    .expect("known kernel ceiling must still allow the best achievable lift");
    assert_eq!(change.effective_soft, 4_096);
    assert_eq!(writes.last(), Some(&4_096));
}

#[test]
fn nofile_kernel_and_hard_ceilings_are_both_respected() {
    let mut writes = Vec::new();
    let change = raise_open_file_soft_limit_with(
        8_192,
        || Ok((256, Some(6_144))),
        |target| {
            writes.push(target);
            Ok(())
        },
        Some(4_096),
    )
    .unwrap();
    assert_eq!(change.effective_soft, 4_096);
    assert_eq!(writes, [4_096]);
}

#[test]
fn nofile_getrlimit_failure_does_not_attempt_a_write() {
    let error = raise_open_file_soft_limit_with(
        8_192,
        || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        |_| panic!("unreadable limits cannot authorize a write"),
        None,
    )
    .unwrap_err();
    assert_eq!(error.error.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn nofile_setrlimit_failure_retains_limit_context_for_startup_diagnostic() {
    let mut writes = 0;
    let error = raise_open_file_soft_limit_with(
        8_192,
        || Ok((256, Some(16_384))),
        |_| {
            writes += 1;
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        },
        None,
    )
    .unwrap_err();
    assert_eq!(writes, 1);
    let message = error.to_string();
    for fact in ["8192", "256", "16384"] {
        assert!(
            message.contains(fact),
            "missing limit fact {fact}: {message}"
        );
    }
}

#[test]
fn nofile_already_at_low_ceiling_reports_unmet_request() {
    let mut notices = Vec::new();
    configure_process_open_file_limit_with(
        8_192,
        |_| {
            Ok(OpenFileLimitChange {
                previous_soft: 4_096,
                effective_soft: 4_096,
                hard_limit: Some(4_096),
                kernel_ceiling: None,
            })
        },
        |line| notices.push(line),
    );
    assert_eq!(
        notices.len(),
        1,
        "an unmet request must not be silently ignored"
    );
    for fact in ["8192", "4096", "hard"] {
        assert!(notices[0].contains(fact));
    }
}

#[test]
fn nofile_unreadable_limits_report_unknown_effective_and_ceiling() {
    let mut notices = Vec::new();
    configure_process_open_file_limit_with(
        8_192,
        |requested| {
            raise_open_file_soft_limit_with(
                requested,
                || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                |_| panic!("an unreadable limit cannot authorize a write"),
                None,
            )
        },
        |line| notices.push(line),
    );
    assert_eq!(notices.len(), 1);
    for fact in ["8192", "effective", "unknown", "hard"] {
        assert!(notices[0].contains(fact));
    }
}

#[test]
fn nofile_satisfied_inherited_limit_is_quiet() {
    let mut notices = Vec::new();
    configure_process_open_file_limit_with(
        8_192,
        |_| {
            Ok(OpenFileLimitChange {
                previous_soft: 16_384,
                effective_soft: 16_384,
                hard_limit: None,
                kernel_ceiling: None,
            })
        },
        |line| notices.push(line),
    );
    assert!(notices.is_empty());
}

#[test]
fn nofile_docs_cross_link_runtime_harness_and_shared_codex_policies() {
    let architecture = include_str!("../docs/architecture.md");
    let tests = include_str!("../docs/test.md");
    let shared = include_str!("../docs/features/shared-codex-app-server.md");
    assert!(architecture.contains("1,048,576") || architecture.contains("1048576"));
    assert!(architecture.contains("TERMAL_TEST_FD_LIMIT"));
    assert!(architecture.contains("features/shared-codex-app-server.md"));
    assert!(architecture.contains("test.md"));
    assert!(tests.contains("TERMAL_NOFILE_LIMIT"));
    assert!(tests.contains("architecture.md#process-descriptor-capacity"));
    assert!(tests.contains("features/shared-codex-app-server.md"));
    assert!(shared.contains("TERMAL_NOFILE_LIMIT"));
    assert!(shared.contains("../architecture.md#process-descriptor-capacity"));
    assert!(shared.contains("../test.md"));
}

#[test]
fn nofile_hidden_kernel_ceiling_search_finds_best_lift_without_lowering() {
    let mut effective = 256;
    let mut writes = Vec::new();
    let change = raise_open_file_soft_limit_with(
        8_192,
        || Ok((256, None)),
        |target| {
            assert!(
                target > effective,
                "each probe must be above the last successful lift"
            );
            writes.push(target);
            if target > 3_001 {
                Err(std::io::ErrorKind::InvalidInput.into())
            } else {
                effective = target;
                Ok(())
            }
        },
        None,
    )
    .unwrap();
    assert_eq!(change.effective_soft, 3_001);
    assert_eq!(effective, 3_001);
    assert!(writes.len() <= 21);
}

#[test]
fn nofile_kernel_rejecting_every_lift_reports_original_effective_limit() {
    let mut writes = 0;
    let error = raise_open_file_soft_limit_with(
        8_192,
        || Ok((256, None)),
        |target| {
            assert!(target > 256);
            writes += 1;
            Err(std::io::ErrorKind::InvalidInput.into())
        },
        None,
    )
    .unwrap_err();
    assert_eq!(error.limits, Some((256, None)));
    assert!(writes <= 21);
}

#[test]
fn nofile_fallback_permission_failure_reports_last_success_not_target() {
    let mut accepted = 256;
    let error = raise_open_file_soft_limit_with(
        8_192,
        || Ok((256, Some(16_384))),
        |target| {
            if target >= 5_000 {
                Err(std::io::ErrorKind::InvalidInput.into())
            } else if accepted > 256 {
                Err(std::io::ErrorKind::PermissionDenied.into())
            } else {
                accepted = target;
                Ok(())
            }
        },
        None,
    )
    .unwrap_err();
    assert!(accepted > 256);
    assert_eq!(error.limits, Some((accepted, Some(16_384))));
    assert_eq!(error.error.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn nofile_success_uses_readback_instead_of_assuming_requested_effect() {
    let planned = OpenFileLimitChange {
        previous_soft: 256,
        effective_soft: 8_192,
        hard_limit: None,
        kernel_ceiling: Some(4_096),
    };
    let actual =
        finalize_open_file_limit_change(8_192, Ok(planned), || Ok((4_096, Some(16_384)))).unwrap();
    assert_eq!(actual.effective_soft, 4_096);
    assert_eq!(actual.hard_limit, Some(16_384));
}

#[test]
fn nofile_failed_readback_reports_unknown_effective_not_planned_success() {
    let planned = OpenFileLimitChange {
        previous_soft: 256,
        effective_soft: 4_096,
        hard_limit: None,
        kernel_ceiling: Some(4_096),
    };
    let error = finalize_open_file_limit_change(8_192, Ok(planned), || {
        Err(std::io::ErrorKind::PermissionDenied.into())
    })
    .unwrap_err();
    assert_eq!(error.limits, None);
    assert_eq!(error.requested, 8_192);
    let notice = error.to_string();
    assert!(notice.contains("effective unknown"));
    assert!(notice.contains("kernel 4096"));
}

#[test]
fn nofile_override_cap_emits_requested_and_policy_maximum() {
    let (value, notices) = selected(Ok("1048577".into()));
    assert_eq!(value, 1_048_576);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("1048577"));
    assert!(notices[0].contains("1048576"));
}

#[test]
fn nofile_inherited_high_soft_limit_still_reports_lower_kernel_ceiling() {
    let mut notices = Vec::new();
    configure_process_open_file_limit_with(
        8_192,
        |_| {
            Ok(OpenFileLimitChange {
                previous_soft: 16_384,
                effective_soft: 16_384,
                hard_limit: None,
                kernel_ceiling: Some(4_096),
            })
        },
        |line| notices.push(line),
    );
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("warning"));
    assert!(notices[0].contains("kernel 4096"));
    assert!(notices[0].contains("effective 16384"));
}

#[test]
fn nofile_mutation_failure_diagnostics_use_final_not_initial_limits() {
    let current = std::cell::Cell::new((256, Some(16_384)));
    let attempt = raise_open_file_soft_limit_with(
        8_192,
        || Ok(current.get()),
        |_| {
            current.set((1_024, Some(4_096)));
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "original mutation error",
            ))
        },
        None,
    );
    let mut final_reads = 0;
    let error = finalize_open_file_limit_change(8_192, attempt, || {
        final_reads += 1;
        Ok(current.get())
    })
    .unwrap_err();
    assert_eq!(final_reads, 1);
    assert_eq!(error.limits, Some((1_024, Some(4_096))));
    assert_eq!(error.error.kind(), std::io::ErrorKind::PermissionDenied);
    let message = error.to_string();
    for fact in [
        "8192",
        "effective 1024",
        "hard 4096",
        "original mutation error",
    ] {
        assert!(message.contains(fact), "{message}");
    }
    assert!(!message.contains("16384"));
}

#[test]
fn nofile_failed_final_read_after_mutation_error_preserves_both_failures() {
    let attempt = raise_open_file_soft_limit_with(
        8_192,
        || Ok((256, Some(16_384))),
        |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "original mutation error",
            ))
        },
        Some(4_096),
    );
    let error = finalize_open_file_limit_change(8_192, attempt, || {
        Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "final snapshot unavailable",
        ))
    })
    .unwrap_err();
    assert_eq!(error.limits, None);
    assert_eq!(error.error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        error.readback_error.as_ref().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    let message = error.to_string();
    for fact in [
        "effective unknown",
        "hard unknown",
        "kernel 4096",
        "original mutation error",
        "final snapshot unavailable",
    ] {
        assert!(message.contains(fact), "{message}");
    }
}

#[test]
fn nofile_initial_read_failure_still_gets_a_final_snapshot_without_writing() {
    let attempt = raise_open_file_soft_limit_with(
        8_192,
        || Err(std::io::ErrorKind::Interrupted.into()),
        |_| panic!("no write without an initial snapshot"),
        None,
    );
    let error =
        finalize_open_file_limit_change(8_192, attempt, || Ok((2_048, Some(4_096)))).unwrap_err();
    assert_eq!(error.error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(error.limits, Some((2_048, Some(4_096))));
}
