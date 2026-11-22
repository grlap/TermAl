// Hyper constructs IoSlice values before calling the socket writer. Split at
// its body boundary, not inside AsyncWrite, where that construction is too late.

use super::*;
const HTTP_WRITE_CHUNK_BYTES: std::num::NonZeroUsize =
    std::num::NonZeroUsize::new(1024 * 1024 * 1024).unwrap();
const NATIVE_WRITE_REPORT_BAND: u64 = 1024 * 1024 * 1024;

pub(crate) async fn chunk_http_response_writes(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let context = HttpPanicContext::from_request(&request);
    let owner = begin_http_response_diagnostic();
    let response = HTTP_REQUEST_DIAGNOSTIC
        .scope(context.clone(), next.run(request))
        .await;
    claim_http_write_diagnostic(owner, &context);
    let metadata = response
        .extensions()
        .get::<OutputSizeObserved>()
        .copied()
        .unwrap_or(OutputSizeObserved {
            producer: OutputProducer::Other,
            revision: None,
        });
    response.map(|body| {
        axum::body::Body::new(
            HttpWriteChunks::new(body, HTTP_WRITE_CHUNK_BYTES)
                .with_output_metadata(metadata)
                .with_diagnostics(owner, context),
        )
    })
}

/// One retained frame, sliced without copying or waiting for more input.
/// Headers, trailers, errors and the total body length are not changed.
pub(crate) struct HttpWriteChunks {
    pub(crate) inner: axum::body::Body,
    pub(crate) pending: axum::body::Bytes,
    pub(crate) limit: std::num::NonZeroUsize,
    pub(crate) diagnostics: Option<(Option<u64>, HttpPanicContext)>,
    pub(crate) observation: NativeWriteObservation,
    encoded_len: Option<u64>,
}

impl HttpWriteChunks {
    pub(crate) fn new(inner: axum::body::Body, limit: std::num::NonZeroUsize) -> Self {
        let encoded_len = http_body::Body::size_hint(&inner).exact();
        Self {
            inner,
            pending: axum::body::Bytes::new(),
            limit,
            diagnostics: None,
            encoded_len,
            observation: NativeWriteObservation::new(
                OutputSizeObserved {
                    producer: OutputProducer::Other,
                    revision: None,
                },
                NATIVE_WRITE_REPORT_BAND,
                Some(report_native_write_size),
            ),
        }
    }

    fn with_diagnostics(mut self, owner: Option<u64>, context: HttpPanicContext) -> Self {
        self.diagnostics = Some((owner, context));
        self
    }

    pub(crate) fn with_output_metadata(mut self, metadata: OutputSizeObserved) -> Self {
        self.observation.metadata = metadata;
        self
    }
}

impl http_body::Body for HttpWriteChunks {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::task::Poll;
        let this = self.get_mut();
        if let Some(bytes) = this.encoded_len.take() {
            // The final encoded body's exact hint is known before its first
            // data poll. Unknown streams still use the incremental lower bound.
            this.observation.observe_known(bytes);
        }
        if !this.pending.is_empty() {
            if let Some((owner, context)) = &this.diagnostics {
                claim_http_write_diagnostic(*owner, context);
            }
            let len = this.pending.len().min(this.limit.get());
            return Poll::Ready(Some(Ok(http_body::Frame::data(this.pending.split_to(len)))));
        }
        let frame = if let Some((_, context)) = &this.diagnostics {
            HTTP_REQUEST_DIAGNOSTIC.sync_scope(context.clone(), || {
                std::pin::Pin::new(&mut this.inner).poll_frame(cx)
            })
        } else {
            std::pin::Pin::new(&mut this.inner).poll_frame(cx)
        };
        if let Poll::Ready(Some(Ok(frame))) = &frame {
            if let Some((owner, context)) = &this.diagnostics {
                claim_http_write_diagnostic(*owner, context);
            }
            if let Some(bytes) = frame.data_ref() {
                // Observe the ORIGINAL frame before publication/slicing. A retained
                // suffix never emits again. Unknown streams report a lower bound.
                this.observation
                    .observe(bytes.len() as u64, this.inner.is_end_stream());
            }
        }
        match frame {
            Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                Ok(mut data) if data.len() > this.limit.get() => {
                    let first = data.split_to(this.limit.get());
                    this.pending = data;
                    Poll::Ready(Some(Ok(http_body::Frame::data(first))))
                }
                Ok(data) => Poll::Ready(Some(Ok(http_body::Frame::data(data)))),
                Err(frame) => Poll::Ready(Some(Ok(frame))),
            },
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.pending.is_empty() && http_body::Body::is_end_stream(&self.inner)
    }

    fn size_hint(&self) -> http_body::SizeHint {
        let inner = http_body::Body::size_hint(&self.inner);
        let pending = self.pending.len() as u64;
        let mut hint = http_body::SizeHint::new();
        hint.set_lower(inner.lower().saturating_add(pending));
        if let Some(upper) = inner.upper().and_then(|upper| upper.checked_add(pending)) {
            hint.set_upper(upper);
        }
        hint
    }
}

#[derive(Clone, Copy)]
pub(crate) struct NativeWriteSize {
    pub(crate) metadata: OutputSizeObserved,
    pub(crate) bytes: u64,
    pub(crate) complete: bool,
    pub(crate) saturated: bool,
}

fn report_native_write_size(size: NativeWriteSize) {
    observe_output_size(
        size.metadata.producer,
        OutputSizeStage::NativePreWrite,
        size.bytes,
        size.complete,
        size.saturated,
        size.metadata.revision,
    );
}

pub(crate) struct NativeWriteObservation {
    pub(crate) metadata: OutputSizeObserved,
    pub(crate) count: OutputByteCount,
    pub(crate) band: u64,
    pub(crate) emitted: bool,
    pub(crate) report: Option<fn(NativeWriteSize)>,
}

impl NativeWriteObservation {
    pub(crate) fn new(
        metadata: OutputSizeObserved,
        band: u64,
        report: Option<fn(NativeWriteSize)>,
    ) -> Self {
        Self {
            metadata,
            count: OutputByteCount::default(),
            band,
            emitted: false,
            report,
        }
    }

    fn observe_known(&mut self, bytes: u64) {
        if !self.emitted && bytes >= self.band {
            self.emitted = true;
            if let Some(report) = self.report {
                report(NativeWriteSize {
                    metadata: self.metadata,
                    bytes,
                    complete: true,
                    saturated: false,
                });
            }
        }
    }

    pub(crate) fn observe(&mut self, bytes: u64, complete: bool) {
        self.count.add(bytes);
        if !self.emitted && self.count.bytes >= self.band {
            self.emitted = true;
            if let Some(report) = self.report {
                report(NativeWriteSize {
                    metadata: self.metadata,
                    bytes: self.count.bytes,
                    complete,
                    saturated: self.count.saturated,
                });
            }
        }
    }
}
