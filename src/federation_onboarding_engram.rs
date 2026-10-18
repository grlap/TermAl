// Owns the profile-gated external Engram verification command adapter.
// Does not implement Engram checks or interpret task-sync authority. New O1
// boundary; commands are configured only by the local operator, never over L1.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationEngramProbeCommand {
    binary: PathBuf,
    args: Vec<String>,
    project_root: PathBuf,
    #[serde(default)]
    json_summary: bool,
}

impl FederationEngramProbeCommand {
    fn validate(&self) -> Result<(), ApiError> {
        if !self.binary.is_absolute()
            || !self.project_root.is_absolute()
            || self.args.len() > 32
            || self
                .args
                .iter()
                .any(|arg| arg.len() > 4096 || arg.contains('\0'))
        {
            return Err(ApiError::bad_request(
                "invalid local Engram verification command",
            ));
        }
        Ok(())
    }
}

fn run_federation_engram_probe(
    config: &FederationEngramProbeCommand,
) -> FederationEngramOnboardingOutcome {
    if config.validate().is_err() {
        return FederationEngramOnboardingOutcome::unavailable();
    }
    #[cfg(windows)]
    let mut command = windows_launch::LaunchSpec::new(&config.binary);
    #[cfg(not(windows))]
    let mut command = Command::new(&config.binary);
    command.args(&config.args).current_dir(&config.project_root);
    let result = run_bounded_read_command(
        &mut command,
        std::time::Instant::now() + Duration::from_secs(2),
        4096,
        true,
    );
    let Ok(output) = result else {
        return FederationEngramOnboardingOutcome::unavailable();
    };
    let summary = if config.json_summary {
        match serde_json::from_slice::<FederationEngramProbeSummary>(&output.stdout) {
            Ok(summary)
                if summary.checks.len() <= 32
                    && summary.checks.iter().all(|check| {
                        check.len() <= 160 && !check.chars().any(char::is_control)
                    }) =>
            {
                Some(summary)
            }
            _ => {
                return FederationEngramOnboardingOutcome {
                    status: FederationEngramProbeStatus::Fail,
                    exit_code: output.status.code(),
                    summary: None,
                    reason: "Engram verification summary invalid".to_owned(),
                };
            }
        }
    } else {
        None
    };
    let passed = output.status.success() && summary.as_ref().is_none_or(|summary| summary.passed);
    FederationEngramOnboardingOutcome {
        status: if passed {
            FederationEngramProbeStatus::Pass
        } else {
            FederationEngramProbeStatus::Fail
        },
        exit_code: output.status.code(),
        summary,
        reason: if passed {
            "Engram external verification passed"
        } else {
            "Engram external verification failed"
        }
        .to_owned(),
    }
}
