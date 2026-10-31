// Observation of existing encoded buffers, never an output policy. The body
// adapter moves the original frames unchanged and preserves their backpressure.
// Producers only update fixed atomic slots; the existing lifecycle thread
// drains them into its bounded journal, so delivery never waits for logging.

const OUTPUT_SIZE_BANDS: [u64; 3] = [16 * 1024 * 1024, 64 * 1024 * 1024, 1024 * 1024 * 1024];

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum OutputProducer {
    State,
    History,
    Session,
    Mailbox,
    Board,
    Federation,
    Terminal,
    StaticFile,
    Other,
}

impl OutputProducer {
    const ALL: [Self; 9] = [
        Self::State,
        Self::History,
        Self::Session,
        Self::Mailbox,
        Self::Board,
        Self::Federation,
        Self::Terminal,
        Self::StaticFile,
        Self::Other,
    ];

    fn for_route(route: Option<&str>) -> Self {
        match route {
            Some("/api/state" | "/api/events") => Self::State,
            Some(path) if path.starts_with("/federation/") => Self::Federation,
            Some(path) if path.contains("/history") => Self::History,
            Some("/api/sessions/{id}") => Self::Session,
            Some(path) if path.contains("/mailbox") => Self::Mailbox,
            Some(path) if path.contains("/board") || path.contains("/response-board") => {
                Self::Board
            }
            Some("/api/terminal/run/stream") => Self::Terminal,
            None => Self::StaticFile,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum OutputSizeStage {
    HttpBodyBeforeCompression,
    SseFrameBeforeCompression,
    NativePreWrite,
}

impl OutputSizeStage {
    const ALL: [Self; 3] = [
        Self::HttpBodyBeforeCompression,
        Self::SseFrameBeforeCompression,
        Self::NativePreWrite,
    ];
}

#[derive(Clone, Copy, Debug, Default)]
struct OutputByteCount {
    bytes: u64,
    saturated: bool,
}

impl OutputByteCount {
    fn add(&mut self, bytes: u64) {
        let (sum, overflow) = self.bytes.overflowing_add(bytes);
        self.bytes = if overflow { u64::MAX } else { sum };
        self.saturated |= overflow;
    }
}

#[derive(Debug)]
struct OutputSizeSlot {
    observations: AtomicU64,
    max_bytes: AtomicU64,
    max_revision: AtomicU64,
    has_revision: AtomicBool,
    saturated: AtomicBool,
}

impl OutputSizeSlot {
    const fn new() -> Self {
        Self {
            observations: AtomicU64::new(0),
            max_bytes: AtomicU64::new(0),
            max_revision: AtomicU64::new(0),
            has_revision: AtomicBool::new(false),
            saturated: AtomicBool::new(false),
        }
    }

    fn observe(&self, bytes: u64, saturated: bool, revision: Option<u64>) {
        self.max_bytes.fetch_max(bytes, Ordering::Relaxed);
        if let Some(revision) = revision {
            self.max_revision.fetch_max(revision, Ordering::Relaxed);
            self.has_revision.store(true, Ordering::Relaxed);
        }
        if saturated {
            self.saturated.store(true, Ordering::Relaxed);
        }
        let mut count = self.observations.load(Ordering::Relaxed);
        loop {
            match self.observations.compare_exchange_weak(
                count,
                count.saturating_add(1),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
    }

    fn take_report(
        &self,
        producer: OutputProducer,
        stage: OutputSizeStage,
        complete: bool,
        band_bytes: u64,
    ) -> Option<OutputSizeReport> {
        let count = self.observations.swap(0, Ordering::AcqRel);
        (count != 0).then(|| OutputSizeReport {
            producer,
            stage,
            complete,
            band_bytes,
            observations_since_drain: count,
            max_bytes_observed: self.max_bytes.load(Ordering::Relaxed),
            max_revision_observed: self
                .has_revision
                .load(Ordering::Relaxed)
                .then(|| self.max_revision.load(Ordering::Relaxed)),
            counter_saturated: self.saturated.load(Ordering::Relaxed) || count == u64::MAX,
        })
    }
}

const OUTPUT_SIZE_SLOT_COUNT: usize =
    OutputProducer::ALL.len() * OutputSizeStage::ALL.len() * 2 * OUTPUT_SIZE_BANDS.len();
static OUTPUT_SIZE_SLOTS: [OutputSizeSlot; OUTPUT_SIZE_SLOT_COUNT] =
    [const { OutputSizeSlot::new() }; OUTPUT_SIZE_SLOT_COUNT];

fn output_size_slot(
    producer: OutputProducer,
    stage: OutputSizeStage,
    complete: bool,
    band: usize,
) -> usize {
    (((producer as usize * OutputSizeStage::ALL.len() + stage as usize) * 2
        + usize::from(complete))
        * OUTPUT_SIZE_BANDS.len())
        + band
}

/// Reusable by the native pre-write seam. Sizes are observations, never limits.
/// No formatted keys, payloads, identifiers, allocation, I/O or locks here.
fn observe_output_size(
    producer: OutputProducer,
    stage: OutputSizeStage,
    bytes: u64,
    complete: bool,
    saturated: bool,
    revision: Option<u64>,
) {
    if let Some(band) = OUTPUT_SIZE_BANDS
        .iter()
        .rposition(|threshold| bytes >= *threshold)
    {
        OUTPUT_SIZE_SLOTS[output_size_slot(producer, stage, complete, band)]
            .observe(bytes, saturated, revision);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct OutputSizeReport {
    producer: OutputProducer,
    stage: OutputSizeStage,
    complete: bool,
    band_bytes: u64,
    observations_since_drain: u64,
    // Lifetime maxima for this fixed slot, not falsely correlated with a
    // particular observation in a concurrent heartbeat interval.
    max_bytes_observed: u64,
    max_revision_observed: Option<u64>,
    counter_saturated: bool,
}

fn drain_output_size_reports(mut report: impl FnMut(OutputSizeReport)) {
    for producer in OutputProducer::ALL {
        for stage in OutputSizeStage::ALL {
            for complete in [false, true] {
                for (band, band_bytes) in OUTPUT_SIZE_BANDS.into_iter().enumerate() {
                    let slot =
                        &OUTPUT_SIZE_SLOTS[output_size_slot(producer, stage, complete, band)];
                    if let Some(value) = slot.take_report(producer, stage, complete, band_bytes) {
                        report(value);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct OutputSizeObserved {
    producer: OutputProducer,
    revision: Option<u64>,
}

#[derive(Clone, Copy)]
struct OutputSizeRevision(u64);

struct OutputSizeBody {
    inner: axum::body::Body,
    metadata: OutputSizeObserved,
    sse: bool,
    count: OutputByteCount,
    finished: bool,
}

impl OutputSizeBody {
    fn finish(&mut self, complete: bool) {
        if !self.finished {
            self.finished = true;
            observe_output_size(
                self.metadata.producer,
                OutputSizeStage::HttpBodyBeforeCompression,
                self.count.bytes,
                complete,
                self.count.saturated,
                self.metadata.revision,
            );
        }
    }
}

impl http_body::Body for OutputSizeBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::task::Poll;
        let this = self.get_mut();
        let result = std::pin::Pin::new(&mut this.inner).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(bytes) = frame.data_ref() {
                    let previous = this.count.bytes;
                    this.count.add(bytes.len() as u64);
                    if this.sse {
                        // Axum SseBody emits exactly one finalized Event per
                        // data frame (including keepalives); no body parsing,
                        // copying or second serialization is necessary.
                        observe_output_size(
                            this.metadata.producer,
                            OutputSizeStage::SseFrameBeforeCompression,
                            bytes.len() as u64,
                            true,
                            false,
                            this.metadata.revision,
                        );
                    }
                    if OUTPUT_SIZE_BANDS
                        .iter()
                        .any(|band| previous < *band && this.count.bytes >= *band)
                    {
                        observe_output_size(
                            this.metadata.producer,
                            OutputSizeStage::HttpBodyBeforeCompression,
                            this.count.bytes,
                            false,
                            this.count.saturated,
                            this.metadata.revision,
                        );
                    }
                }
                if this.inner.is_end_stream() {
                    this.finish(true);
                }
            }
            Poll::Ready(Some(Err(_))) => this.finish(false),
            Poll::Ready(None) => this.finish(true),
            Poll::Pending => {}
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        http_body::Body::is_end_stream(&self.inner)
    }
    fn size_hint(&self) -> http_body::SizeHint {
        http_body::Body::size_hint(&self.inner)
    }
}

impl Drop for OutputSizeBody {
    fn drop(&mut self) {
        self.finish(http_body::Body::is_end_stream(&self.inner));
    }
}

fn observe_http_response(mut response: Response, producer: OutputProducer) -> Response {
    if response.extensions().get::<OutputSizeObserved>().is_some() {
        return response;
    }
    let revision = response
        .extensions()
        .get::<OutputSizeRevision>()
        .map(|revision| revision.0);
    let sse = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    response
        .extensions_mut()
        .insert(OutputSizeObserved { producer, revision });
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        axum::body::Body::new(OutputSizeBody {
            inner: body,
            metadata: OutputSizeObserved { producer, revision },
            sse,
            count: OutputByteCount::default(),
            finished: false,
        }),
    )
}

async fn output_size_http_observer(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let producer = OutputProducer::for_route(
        request
            .extensions()
            .get::<axum::extract::MatchedPath>()
            .map(|path| path.as_str()),
    );
    observe_http_response(next.run(request).await, producer)
}

#[cfg(test)]
#[path = "tests/output_size.rs"]
mod output_size_tests;
