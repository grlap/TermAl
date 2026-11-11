// Fixed-label boot measurements. Nested session phases are aggregated across
// rows so startup never prints session IDs, paths or persisted payloads.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BootStateLoadPhase {
    Open,
    SchemaMetadata,
    Sessions,
    SessionDecode,
    TranscriptTails,
    PromptHistories,
    Delegations,
    SessionAdoption,
    SessionRebuild,
    DerivedRebuild,
    FederationBind,
}

impl BootStateLoadPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::SchemaMetadata => "schema_metadata",
            Self::Sessions => "sessions_total",
            Self::SessionDecode => "session_decode",
            Self::TranscriptTails => "transcript_tails",
            Self::PromptHistories => "prompt_histories",
            Self::Delegations => "delegations_inline",
            Self::SessionAdoption => "session_adoption",
            Self::SessionRebuild => "session_validate_rebuild",
            Self::DerivedRebuild => "derived_rebuild",
            Self::FederationBind => "federation_bind",
        }
    }
}

struct BootStateLoadTimings {
    enabled: bool,
    measurements: BTreeMap<BootStateLoadPhase, BootStateLoadMeasurement>,
}

#[derive(Default)]
struct BootStateLoadMeasurement {
    elapsed: std::time::Duration,
    calls: usize,
}

impl BootStateLoadTimings {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            measurements: BTreeMap::new(),
        }
    }

    fn start(&self) -> Option<std::time::Instant> {
        self.enabled.then(std::time::Instant::now)
    }

    fn finish(&mut self, phase: BootStateLoadPhase, started: Option<std::time::Instant>) {
        if let Some(started) = started {
            let elapsed = started.elapsed();
            let measurement = self.measurements.entry(phase).or_default();
            measurement.elapsed += elapsed;
            measurement.calls += 1;
        }
    }

    fn scoped(&mut self, phase: BootStateLoadPhase) -> BootStateLoadTiming<'_> {
        let started = self.start();
        BootStateLoadTiming {
            timings: self,
            phase,
            started,
        }
    }

    fn log(&self) {
        // sessions_total includes decode, tails and histories; the remaining
        // phases are disjoint from that aggregate. Deferred delegation work
        // is outside state_load and emits no inline-loading measurement.
        for (phase, measurement) in &self.measurements {
            eprintln!(
                "startup> phase=state_load subphase={} elapsed_us={} calls={}",
                phase.label(),
                measurement.elapsed.as_micros(),
                measurement.calls
            );
        }
    }
}

// Scope exit records both successful and failed attempts before errors escape.
struct BootStateLoadTiming<'a> {
    timings: &'a mut BootStateLoadTimings,
    phase: BootStateLoadPhase,
    started: Option<std::time::Instant>,
}

impl Drop for BootStateLoadTiming<'_> {
    fn drop(&mut self) {
        self.timings.finish(self.phase, self.started);
    }
}
