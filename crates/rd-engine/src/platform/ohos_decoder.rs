//! New, metadata/bitstream-only OHOS Surface decoder, not the legacy runtime.
//!
//! Integration: std plus existing Tokio Notify/cancellation support. Link libraries:
//! native_media_codecbase, native_media_core, native_media_vdec, native_window.
//! Installed headers: API26 (26.0.0.32); all bound APIs introduced <=12, target
//! compatibility API23. Original SDK23/App/API24-device validation is pending.
//! References: developer.huawei.com/consumer/cn/doc/harmonyos-references/
//! capi-native-avcodec-videodecoder-h and capi-native-avbuffer-h.
//!
//! One persistent worker per stream. Every callback, submission and close wakes
//! the SAME condition variable. No SDK call holds its queue mutex. No pixel
//! buffer address is read: GetAddr is used ONLY for compressed decoder INPUT.
//! Close explicitly cancels queued access units; normal playback never flushes
//! or drops accepted P frames. For a new stream, close the old decoder first.

use std::sync::Arc;

/// Implemented only by the trusted HAR Surface owner.
///
/// # Safety
/// The surface ID must remain stable and identify a live Surface. Retaining this lease must keep
/// that Surface alive (including across ArkUI teardown requests) until the last
/// lease is dropped. Drop must be safe on the decoder worker. On Destroy failure
/// the lease is retained in bounded quarantine; HAR must not invalidate it.
pub unsafe trait SurfaceLease: Send + Sync + 'static {
    fn surface_id(&self) -> u64;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
}

#[derive(Debug, Clone, Copy)]
pub struct DecoderConfig {
    pub codec: VideoCodec,
    /// Maximum expected coded geometry for this decoder instance.
    pub width: i32,
    pub height: i32,
    /// Capture rate the stream is encoded at.
    ///
    /// The platform's video variable refresh rate feature reads this to decide
    /// what the panel should run at, so a decoder that does not know its own
    /// frame rate cannot use the feature at all.
    pub frame_rate: u32,
    pub max_queued_units: usize,
    pub max_queued_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessUnitKind {
    CodecConfig,
    Frame { key: bool },
    EndOfStream,
}

#[derive(Debug)]
pub struct AccessUnit {
    /// Complete compressed access unit; Annex-B framing supplied by the protocol
    /// adapter. No pixel data. CodecConfig carries codec parameter sets only.
    pub bytes: Vec<u8>,
    pub pts_us: i64,
    pub kind: AccessUnitKind,
    /// When the unit was handed to the decoder, used to time the path from
    /// arrival to the render submission. `None` means it was never submitted
    /// through [`Shared::submit`], which is how a unit built by a test is
    /// distinguished from one that travelled the real path.
    pub accepted_at: Option<std::time::Instant>,
    /// When the unit left the admission queue.
    ///
    /// The span from `accepted_at` to here is queue wait, and it is the part of
    /// the path that grows without bound when the peer sends faster than this
    /// device decodes. The span from here to the frame's submission is the work:
    /// the decode itself plus the wait for an output buffer.
    pub dequeued_at: Option<std::time::Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecoderError {
    UnsupportedPlatform,
    InvalidConfig,
    InvalidAccessUnit,
    Backpressure,
    AccessUnitExceedsQueueLimit { size: usize, limit: usize },
    Closed,
    EndOfStreamAlreadySubmitted,
    NoHardwareDecoder,
    NativeReturnedNull { api: &'static str },
    InvalidNativeBuffer,
    InputTooLarge { size: usize, capacity: i32 },
    Native { api: &'static str, code: i32 },
    CallbackOverflow,
    DuplicateBufferIndex,
    OwnerLimit,
    QuarantinePresent,
    DestroyFailed { code: i32 },
    WorkerStartFailed,
    WorkerPanicked,
}

/// Rejection returns ownership, so the caller can preserve stream ordering on
/// backpressure. Retrying must precede any later access unit on that stream.
#[derive(Debug)]
pub struct RejectedSubmission {
    pub reason: DecoderError,
    pub unit: AccessUnit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecoderStats {
    pub accepted_units: u64,
    pub pushed_units: u64,
    /// Successful RenderOutputBuffer submissions, NOT physical presentation.
    pub render_submissions: u64,
    /// Total time from a unit's arrival to its render submission, in
    /// microseconds. Divided by `timed_units` this is the average pipeline
    /// latency the decoder is responsible for.
    pub pipeline_micros_total: u64,
    /// The worst single pipeline latency seen, in microseconds. An average hides
    /// the stalls that a viewer notices as a stutter, so the peak is reported
    /// alongside it.
    pub pipeline_micros_peak: u64,
    /// Units that carried a timestamp and contributed to the totals above.
    pub timed_units: u64,
    /// Arrival-to-render-submission latency, in microseconds. This is the path
    /// the operator waits through: a unit's arrival, its decode, and the
    /// submission of the frame it produced. `pipeline_micros_*` above measures
    /// the submission call alone and is not this figure.
    pub render_micros_total: u64,
    /// The worst single arrival-to-render latency of the window, in
    /// microseconds. An average hides the stalls a viewer notices as a stutter.
    pub render_micros_peak: u64,
    /// Frames that carried a matching arrival stamp and are counted above.
    pub render_timed_units: u64,
    /// Time units spent waiting for a place in the admission queue, in
    /// microseconds. This is not decode work and must not be reported as it.
    pub queue_micros_total: u64,
    /// Units that were timed in the queue and are counted above.
    pub queue_timed_units: u64,
    pub non_frame_outputs: u64,
    pub format_change_notifications: u64,
    pub cancelled_units: u64,
    pub end_of_stream_output: bool,
    /// Failed/unproven native destruction retained userdata AND the HAR lease.
    pub quarantined: bool,
    pub closed: bool,
    pub failure: Option<DecoderError>,
}

pub struct SurfaceDecoder {
    #[cfg(target_env = "ohos")]
    shared: Arc<engine::Shared>,
    #[cfg(target_env = "ohos")]
    worker: Option<std::thread::JoinHandle<Result<DecoderStats, DecoderError>>>,
    #[cfg(target_env = "ohos")]
    codec_name: String,
}

/// Observes worker state without owning/joining the native decoder. Safe to keep
/// in the HAR-facing snapshot after explicit decoder teardown.
#[derive(Clone)]
pub struct DecoderObserver {
    #[cfg(target_env = "ohos")]
    shared: Arc<engine::Shared>,
}

impl DecoderObserver {
    pub async fn wait_stopped(&self) -> Result<(), DecoderError> {
        #[cfg(target_env = "ohos")]
        loop {
            let changed = self.shared.capacity.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let stats = self.shared.stats();
            if let Some(error) = stats.failure {
                return Err(error);
            }
            if stats.closed || stats.quarantined || stats.end_of_stream_output {
                return Err(DecoderError::Closed);
            }
            changed.await;
        }
        #[cfg(not(target_env = "ohos"))]
        {
            Err(DecoderError::UnsupportedPlatform)
        }
    }
    pub fn stats(&self) -> Result<DecoderStats, DecoderError> {
        #[cfg(target_env = "ohos")]
        {
            Ok(self.shared.stats())
        }
        #[cfg(not(target_env = "ohos"))]
        {
            Err(DecoderError::UnsupportedPlatform)
        }
    }
    pub fn queued_units(&self) -> usize {
        #[cfg(target_env = "ohos")]
        {
            self.shared.queued_units()
        }
        #[cfg(not(target_env = "ohos"))]
        {
            0
        }
    }
    pub fn request_close(&self) {
        #[cfg(target_env = "ohos")]
        self.shared.request_close();
    }
}

impl SurfaceDecoder {
    pub fn observer(&self) -> DecoderObserver {
        DecoderObserver {
            #[cfg(target_env = "ohos")]
            shared: self.shared.clone(),
        }
    }

    /// Capacity includes an access unit currently inside PushInputBuffer. A
    /// notification is registered BEFORE admission to avoid a lost wakeup. No
    /// timer, polling retry or per-frame task. One ordered feeder should call it.
    pub async fn submit_cancellable(
        &self,
        mut unit: AccessUnit,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), RejectedSubmission> {
        #[cfg(target_env = "ohos")]
        loop {
            let ready = self.shared.capacity.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if cancel.is_cancelled() {
                return Err(RejectedSubmission {
                    reason: DecoderError::Closed,
                    unit,
                });
            }
            match self.try_submit(unit) {
                Ok(()) => return Ok(()),
                Err(rejected) if rejected.reason == DecoderError::Backpressure => {
                    unit = rejected.unit
                }
                Err(rejected) => return Err(rejected),
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(RejectedSubmission { reason: DecoderError::Closed, unit }),
                _ = &mut ready => {},
            }
        }
        #[cfg(not(target_env = "ohos"))]
        {
            let _ = (cancel, &mut unit);
            self.try_submit(unit)
        }
    }
    /// Creates by the NAME returned by a hardware-category query, never by a
    /// software-fallback MIME constructor. Does not certify Main10/HDR display.
    pub fn open(config: DecoderConfig, lease: Arc<dyn SurfaceLease>) -> Result<Self, DecoderError> {
        #[cfg(target_env = "ohos")]
        {
            native::open(config, lease)
        }
        #[cfg(not(target_env = "ohos"))]
        {
            let _ = (config, lease);
            Err(DecoderError::UnsupportedPlatform)
        }
    }

    pub fn try_submit(&self, unit: AccessUnit) -> Result<(), RejectedSubmission> {
        #[cfg(target_env = "ohos")]
        {
            self.shared.submit(unit)
        }
        #[cfg(not(target_env = "ohos"))]
        {
            Err(RejectedSubmission {
                reason: DecoderError::UnsupportedPlatform,
                unit,
            })
        }
    }

    pub fn codec_name(&self) -> Result<&str, DecoderError> {
        #[cfg(target_env = "ohos")]
        {
            Ok(&self.codec_name)
        }
        #[cfg(not(target_env = "ohos"))]
        {
            Err(DecoderError::UnsupportedPlatform)
        }
    }

    pub fn stats(&self) -> Result<DecoderStats, DecoderError> {
        #[cfg(target_env = "ohos")]
        {
            Ok(self.shared.stats())
        }
        #[cfg(not(target_env = "ohos"))]
        {
            Err(DecoderError::UnsupportedPlatform)
        }
    }

    /// Nonblocking notification; HAR must still retain Surface until close joins.
    pub fn request_close(&self) {
        #[cfg(target_env = "ohos")]
        self.shared.request_close();
    }

    /// Joins the persistent worker; call off UI/callback threads. Native teardown
    /// has no unsafe timeout/force-free. A stuck SDK call can keep close blocked.
    pub fn close(mut self) -> Result<DecoderStats, DecoderError> {
        self.join()
    }

    fn join(&mut self) -> Result<DecoderStats, DecoderError> {
        #[cfg(target_env = "ohos")]
        {
            self.shared.request_close();
            match self.worker.take() {
                Some(worker) => worker.join().map_err(|_| DecoderError::WorkerPanicked)?,
                None => Ok(self.shared.stats()),
            }
        }
        #[cfg(not(target_env = "ohos"))]
        {
            Err(DecoderError::UnsupportedPlatform)
        }
    }
}

impl Drop for SurfaceDecoder {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

#[cfg(any(target_env = "ohos", test))]
mod engine {
    use super::*;
    use std::{
        collections::{HashSet, VecDeque},
        sync::{Condvar, Mutex, MutexGuard},
    };
    pub const MAX_CALLBACKS: usize = 256;
    pub const MAX_OWNERS: usize = 4;

    pub fn output_flags(flags: u32) -> (bool, bool) {
        // native_avbuffer_info.h: EOS=1, CODEC_DATA=8, DISCARD=16.
        // DISPOSABLE is still a valid frame: never proactively drop it.
        (flags & (1 | 8 | 16) == 0, flags & 1 != 0)
    }

    pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Buffer {
        pub index: u32,
        pub address: usize,
    }
    #[derive(Debug, Clone, Copy)]
    pub enum Event {
        Input(Buffer),
        Output(Buffer),
        StreamChanged,
    }
    pub enum Action {
        Event(Event),
        Submit(Buffer, AccessUnit),
        Stop,
    }

    struct State {
        packets: VecDeque<AccessUnit>,
        events: VecDeque<Event>,
        inputs: VecDeque<Buffer>,
        input_indices: HashSet<u32>,
        output_indices: HashSet<u32>,
        /// Arrival instants of submitted units, awaiting the frame they produce.
        /// Bounded by the submission reservation, so it cannot grow without a
        /// matching output.
        pending_latency: VecDeque<std::time::Instant>,
        // Includes the access unit removed by the worker until Push returns.
        reserved_units: usize,
        reserved_bytes: usize,
        eos_accepted: bool,
        closing: bool,
        stats: DecoderStats,
    }
    pub struct Shared {
        state: Mutex<State>,
        wake: Condvar,
        limits: DecoderConfig,
        pub capacity: tokio::sync::Notify,
    }

    impl Shared {
        pub fn new(limits: DecoderConfig) -> Result<Self, DecoderError> {
            if limits.width <= 0
                || limits.height <= 0
                || limits.max_queued_units == 0
                || limits.max_queued_units > 64
                || limits.max_queued_bytes == 0
                || limits.max_queued_bytes > 64 * 1024 * 1024
            {
                return Err(DecoderError::InvalidConfig);
            }
            Ok(Self {
                state: Mutex::new(State {
                    packets: VecDeque::with_capacity(limits.max_queued_units),
                    events: VecDeque::with_capacity(MAX_CALLBACKS),
                    inputs: VecDeque::with_capacity(MAX_CALLBACKS),
                    input_indices: HashSet::with_capacity(MAX_CALLBACKS),
                    output_indices: HashSet::with_capacity(MAX_CALLBACKS),
                    pending_latency: VecDeque::with_capacity(limits.max_queued_units),
                    reserved_units: 0,
                    reserved_bytes: 0,
                    eos_accepted: false,
                    closing: false,
                    stats: DecoderStats::default(),
                }),
                wake: Condvar::new(),
                limits,
                capacity: tokio::sync::Notify::new(),
            })
        }

        pub fn submit(&self, mut unit: AccessUnit) -> Result<(), RejectedSubmission> {
            unit.accepted_at = Some(std::time::Instant::now());
            unit.dequeued_at = None;
            let mut state = lock(&self.state);
            let invalid = match unit.kind {
                AccessUnitKind::EndOfStream => !unit.bytes.is_empty(),
                _ => unit.bytes.is_empty() || unit.bytes.len() > i32::MAX as usize,
            };
            let reason = if state.closing || state.stats.closed {
                Some(DecoderError::Closed)
            } else if let Some(error) = &state.stats.failure {
                Some(error.clone())
            } else if state.eos_accepted {
                Some(DecoderError::EndOfStreamAlreadySubmitted)
            } else if invalid {
                Some(DecoderError::InvalidAccessUnit)
            } else if unit.bytes.len() > self.limits.max_queued_bytes {
                Some(DecoderError::AccessUnitExceedsQueueLimit {
                    size: unit.bytes.len(),
                    limit: self.limits.max_queued_bytes,
                })
            } else if state.reserved_units >= self.limits.max_queued_units
                || unit.bytes.len()
                    > self
                        .limits
                        .max_queued_bytes
                        .saturating_sub(state.reserved_bytes)
            {
                Some(DecoderError::Backpressure)
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(RejectedSubmission { reason, unit });
            }
            state.eos_accepted = unit.kind == AccessUnitKind::EndOfStream;
            state.reserved_units += 1;
            state.reserved_bytes += unit.bytes.len();
            state.stats.accepted_units += 1;
            state.packets.push_back(unit);
            self.wake.notify_one();
            Ok(())
        }

        // Callback path: bounded metadata enqueue only; no SDK call or waiting
        // for input capacity, no user closures, no per-frame thread spawning.
        pub fn post(&self, event: Event) {
            let mut state = lock(&self.state);
            if state.closing || state.stats.closed || state.stats.failure.is_some() {
                return;
            }
            if state.events.len() >= MAX_CALLBACKS
                || state.input_indices.len() + state.output_indices.len() >= MAX_CALLBACKS
            {
                state.stats.failure = Some(DecoderError::CallbackOverflow);
            } else {
                let duplicate = match event {
                    Event::Input(b) => !state.input_indices.insert(b.index),
                    Event::Output(b) => !state.output_indices.insert(b.index),
                    Event::StreamChanged => false,
                };
                if duplicate {
                    state.stats.failure = Some(DecoderError::DuplicateBufferIndex);
                } else {
                    state.events.push_back(event);
                }
            }
            self.wake.notify_one();
            if state.stats.failure.is_some() {
                self.capacity.notify_waiters();
            }
        }

        pub fn fail(&self, error: DecoderError) {
            let mut state = lock(&self.state);
            if state.stats.failure.is_none() {
                state.stats.failure = Some(error);
            }
            self.wake.notify_one();
            self.capacity.notify_waiters();
        }

        pub fn next(&self) -> Action {
            let mut state = lock(&self.state);
            loop {
                if state.closing || state.stats.failure.is_some() {
                    return Action::Stop;
                }
                // Always service callbacks (including output reclamation) BEFORE
                // waiting for input. Submit does not block on a separate queue.
                if let Some(event) = state.events.pop_front() {
                    return Action::Event(event);
                }
                if !state.inputs.is_empty() && !state.packets.is_empty() {
                    let buffer = state.inputs.pop_front().unwrap();
                    let mut unit = state.packets.pop_front().unwrap();
                    // The unit leaves the queue exactly here, so everything it
                    // waited for since arrival is behind it and what remains is
                    // the decode plus the wait for an output buffer. Splitting
                    // the two is the difference between a decode time and a queue
                    // time; reporting their sum as either one is what made the
                    // figure look absurd.
                    unit.dequeued_at = Some(std::time::Instant::now());
                    return Action::Submit(buffer, unit);
                }
                state = self.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        }

        pub fn input_available(&self, buffer: Buffer) {
            lock(&self.state).inputs.push_back(buffer);
        }
        // Transfer index ownership BEFORE calling Push/Render/Free. Those SDK
        // calls may synchronously recycle the same index through a callback.
        pub fn returning_input(&self, index: u32) {
            lock(&self.state).input_indices.remove(&index);
        }
        pub fn returning_output(&self, index: u32) {
            lock(&self.state).output_indices.remove(&index);
        }
        pub fn submitted(&self, bytes: usize, success: bool) {
            self.submitted_at(bytes, success, None, None)
        }

        /// Record a completed submission, attributing the pipeline latency of the
        /// unit that produced it.
        ///
        /// `elapsed_micros` is measured by the caller, which is the only place
        /// that still has the unit: the callback path deliberately carries
        /// indices rather than payloads, so the moment has to be taken there and
        /// passed in.
        pub fn submitted_at(
            &self,
            bytes: usize,
            success: bool,
            elapsed_micros: Option<u64>,
            queue_micros: Option<u64>,
        ) {
            let mut s = lock(&self.state);
            if success {
                // Queue the dequeue stamp for the frame this submission will
                // produce. Stamping at dequeue rather than at arrival is what
                // keeps the admission wait out of the render figure; this queue
                // is what pairs a rendered frame with the unit behind it.
                s.pending_latency.push_back(std::time::Instant::now());
                if let Some(micros) = queue_micros {
                    s.stats.queue_timed_units = s.stats.queue_timed_units.saturating_add(1);
                    s.stats.queue_micros_total = s.stats.queue_micros_total.saturating_add(micros);
                }
            }
            s.reserved_units -= 1;
            s.reserved_bytes -= bytes;
            if success {
                s.stats.pushed_units += 1;
            } else {
                s.stats.cancelled_units += 1;
            }
            if let Some(micros) = elapsed_micros {
                s.stats.timed_units += 1;
                s.stats.pipeline_micros_total = s.stats.pipeline_micros_total.saturating_add(micros);
                if micros > s.stats.pipeline_micros_peak {
                    s.stats.pipeline_micros_peak = micros;
                }
            }
            self.capacity.notify_waiters();
        }
        pub fn output(&self, frame: bool, eos: bool) {
            let mut s = lock(&self.state);
            if frame {
                s.stats.render_submissions += 1;
                // The buffer carries no timestamp, so a frame is matched to the
                // unit that arrived for it by submission order: the codec hands
                // frames back in the order they were pushed. An empty queue is an
                // output without a matching submission and is not a sample.
                if let Some(arrived) = s.pending_latency.pop_front() {
                    let micros = arrived.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
                    s.stats.render_timed_units = s.stats.render_timed_units.saturating_add(1);
                    s.stats.render_micros_total = s.stats.render_micros_total.saturating_add(micros);
                    if micros > s.stats.render_micros_peak {
                        s.stats.render_micros_peak = micros;
                    }
                }
            } else {
                s.stats.non_frame_outputs += 1;
            }
            if eos {
                s.stats.end_of_stream_output = true;
                s.closing = true;
                self.capacity.notify_waiters();
            }
        }
        pub fn stream_changed(&self) {
            lock(&self.state).stats.format_change_notifications += 1;
        }
        pub fn request_close(&self) {
            lock(&self.state).closing = true;
            self.wake.notify_one();
            self.capacity.notify_waiters();
        }
        pub fn quarantine(&self) {
            let mut state = lock(&self.state);
            state.stats.quarantined = true;
            state.closing = true;
            // Preserve only bounded callback context/Surface/native ownership,
            // not queued compressed data after a failed teardown or unwind.
            state.stats.cancelled_units += state.reserved_units as u64;
            state.reserved_units = 0;
            state.reserved_bytes = 0;
            state.packets.clear();
            state.events.clear();
            state.inputs.clear();
            state.input_indices.clear();
            state.output_indices.clear();
            self.wake.notify_one();
            self.capacity.notify_waiters();
        }
        /// Units waiting for a place in the decoder, right now.
        ///
        /// A gauge rather than a counter: it rises and falls, so it must not be
        /// accumulated like the totals beside it.
        pub fn queued_units(&self) -> usize {
            lock(&self.state).packets.len()
        }
        pub fn stats(&self) -> DecoderStats {
            lock(&self.state).stats.clone()
        }
        pub fn closed(&self) {
            let mut s = lock(&self.state);
            s.closing = true;
            s.stats.cancelled_units += s.packets.len() as u64;
            s.packets.clear();
            s.inputs.clear();
            s.events.clear();
            s.input_indices.clear();
            s.output_indices.clear();
            s.reserved_units = 0;
            s.reserved_bytes = 0;
            s.stats.closed = true;
            self.capacity.notify_waiters();
        }
    }

    #[derive(Default)]
    pub struct OwnerBudget {
        pub live: usize,
        pub quarantined: usize,
    }
    impl OwnerBudget {
        pub fn reserve(&mut self) -> Result<(), DecoderError> {
            if self.quarantined != 0 {
                return Err(DecoderError::QuarantinePresent);
            }
            if self.live >= MAX_OWNERS {
                return Err(DecoderError::OwnerLimit);
            }
            self.live += 1;
            Ok(())
        }
    }
}

#[cfg(target_env = "ohos")]
mod native {
    use super::{engine::*, *};
    use std::{
        ffi::{CStr, c_char, c_void},
        ptr,
        sync::{
            Condvar, Mutex, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    #[repr(C)]
    struct Codec {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    struct Format {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    struct NativeBuffer {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    struct Window {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    struct Capability {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        pts: i64,
        size: i32,
        offset: i32,
        flags: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    #[repr(C)]
    struct Callbacks {
        error: unsafe extern "C" fn(*mut Codec, i32, *mut c_void),
        changed: unsafe extern "C" fn(*mut Codec, *mut Format, *mut c_void),
        input: unsafe extern "C" fn(*mut Codec, u32, *mut NativeBuffer, *mut c_void),
        output: unsafe extern "C" fn(*mut Codec, u32, *mut NativeBuffer, *mut c_void),
    }
    const EOS: u32 = 1;
    const SYNC: u32 = 2;
    const CONFIG: u32 = 8;
    const CLOCK_MONOTONIC: i32 = 1;

    unsafe extern "C" {
        fn clock_gettime(clock_id: i32, time: *mut Timespec) -> i32;
    }

    #[link(name = "native_media_codecbase")]
    unsafe extern "C" {
        fn OH_AVCodec_GetCapabilityByCategory(
            mime: *const c_char,
            encoder: bool,
            category: i32,
        ) -> *mut Capability;
        fn OH_AVCapability_IsHardware(cap: *mut Capability) -> bool;
        fn OH_AVCapability_GetName(cap: *mut Capability) -> *const c_char;
    }
    #[link(name = "native_media_core")]
    unsafe extern "C" {
        static OH_MD_KEY_VIDEO_ENABLE_LOW_LATENCY: *const c_char;
        static OH_MD_KEY_FRAME_RATE: *const c_char;
        static OH_MD_KEY_VIDEO_DECODER_OUTPUT_ENABLE_VRR: *const c_char;
        fn OH_AVFormat_CreateVideoFormat(
            mime: *const c_char,
            width: i32,
            height: i32,
        ) -> *mut Format;
        fn OH_AVFormat_SetIntValue(format: *mut Format, key: *const c_char, value: i32) -> bool;
        fn OH_AVFormat_Destroy(format: *mut Format);
        fn OH_AVBuffer_GetAddr(buffer: *mut NativeBuffer) -> *mut u8;
        fn OH_AVBuffer_GetCapacity(buffer: *mut NativeBuffer) -> i32;
        fn OH_AVBuffer_SetBufferAttr(buffer: *mut NativeBuffer, attr: *const Attr) -> i32;
        fn OH_AVBuffer_GetBufferAttr(buffer: *mut NativeBuffer, attr: *mut Attr) -> i32;
    }
    #[link(name = "native_window")]
    unsafe extern "C" {
        fn OH_NativeWindow_CreateNativeWindowFromSurfaceId(
            id: u64,
            window: *mut *mut Window,
        ) -> i32;
        fn OH_NativeWindow_DestroyNativeWindow(window: *mut Window);
    }
    #[link(name = "native_media_vdec")]
    unsafe extern "C" {
        fn OH_VideoDecoder_CreateByName(name: *const c_char) -> *mut Codec;
        fn OH_VideoDecoder_RegisterCallback(
            codec: *mut Codec,
            callbacks: Callbacks,
            userdata: *mut c_void,
        ) -> i32;
        fn OH_VideoDecoder_Configure(codec: *mut Codec, format: *mut Format) -> i32;
        fn OH_VideoDecoder_SetSurface(codec: *mut Codec, window: *mut Window) -> i32;
        fn OH_VideoDecoder_Prepare(codec: *mut Codec) -> i32;
        fn OH_VideoDecoder_Start(codec: *mut Codec) -> i32;
        fn OH_VideoDecoder_Stop(codec: *mut Codec) -> i32;
        fn OH_VideoDecoder_Destroy(codec: *mut Codec) -> i32;
        fn OH_VideoDecoder_PushInputBuffer(codec: *mut Codec, index: u32) -> i32;
        fn OH_VideoDecoder_RenderOutputBufferAtTime(
            codec: *mut Codec,
            index: u32,
            render_timestamp_ns: i64,
        ) -> i32;
        fn OH_VideoDecoder_FreeOutputBuffer(codec: *mut Codec, index: u32) -> i32;
    }

    fn check(api: &'static str, code: i32) -> Result<(), DecoderError> {
        if code == 0 {
            Ok(())
        } else {
            Err(DecoderError::Native { api, code })
        }
    }

    fn monotonic_time_ns() -> Result<i64, DecoderError> {
        let mut time = Timespec::default();
        check("clock_gettime(CLOCK_MONOTONIC)", unsafe {
            clock_gettime(CLOCK_MONOTONIC, &mut time)
        })?;
        time.tv_sec
            .checked_mul(1_000_000_000)
            .and_then(|seconds| seconds.checked_add(time.tv_nsec))
            .ok_or(DecoderError::Native {
                api: "clock_gettime(CLOCK_MONOTONIC)",
                code: -1,
            })
    }

    struct Context {
        shared: Arc<Shared>,
        active: AtomicUsize,
        idle_lock: Mutex<()>,
        idle: Condvar,
    }
    impl Context {
        fn wait_idle(&self) {
            let mut guard = lock(&self.idle_lock);
            while self.active.load(Ordering::Acquire) != 0 {
                guard = self.idle.wait(guard).unwrap_or_else(|e| e.into_inner());
            }
        }
    }
    // Successful Destroy ends future callback delivery. Also await callbacks
    // already entered before freeing userdata. Failed Destroy NEVER frees it.
    unsafe fn callback(userdata: *mut c_void, f: impl FnOnce(&Shared)) {
        if userdata.is_null() {
            return;
        }
        let context = unsafe { &*(userdata as *const Context) };
        context.active.fetch_add(1, Ordering::AcqRel);
        f(&context.shared);
        let _guard = lock(&context.idle_lock);
        context.active.fetch_sub(1, Ordering::AcqRel);
        context.idle.notify_all();
    }
    unsafe extern "C" fn on_error(_: *mut Codec, code: i32, data: *mut c_void) {
        unsafe {
            callback(data, |s| {
                s.fail(DecoderError::Native {
                    api: "onError",
                    code,
                })
            })
        };
    }
    unsafe extern "C" fn on_changed(_: *mut Codec, _: *mut Format, data: *mut c_void) {
        // Format pointer expires when this callback returns: never enqueue it.
        unsafe { callback(data, |s| s.post(Event::StreamChanged)) };
    }
    unsafe extern "C" fn on_input(
        _: *mut Codec,
        index: u32,
        buffer: *mut NativeBuffer,
        data: *mut c_void,
    ) {
        unsafe {
            callback(data, |s| {
                s.post(Event::Input(Buffer {
                    index,
                    address: buffer as usize,
                }))
            })
        };
    }
    unsafe extern "C" fn on_output(
        _: *mut Codec,
        index: u32,
        buffer: *mut NativeBuffer,
        data: *mut c_void,
    ) {
        unsafe {
            callback(data, |s| {
                s.post(Event::Output(Buffer {
                    index,
                    address: buffer as usize,
                }))
            })
        };
    }

    // Reserve retention capacity BEFORE any native allocation. At most four
    // owners (starting/live/quarantined) exist. Any quarantine latches creation
    // off for the process; no periodic retry and no unbounded leaked userdata.
    struct Gate {
        budget: OwnerBudget,
        retained: Vec<Allocation>,
    }
    fn gate() -> &'static Mutex<Gate> {
        static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();
        GATE.get_or_init(|| {
            Mutex::new(Gate {
                budget: OwnerBudget::default(),
                retained: Vec::with_capacity(MAX_OWNERS),
            })
        })
    }
    struct Permit;
    impl Permit {
        fn acquire() -> Result<Self, DecoderError> {
            lock(gate()).budget.reserve()?;
            Ok(Self)
        }
    }
    impl Drop for Permit {
        fn drop(&mut self) {
            lock(gate()).budget.live -= 1;
        }
    }
    struct Allocation {
        codec: usize,
        window: usize,
        started: bool,
        context: Box<Context>,
        _lease: Arc<dyn SurfaceLease>,
        _permit: Permit,
    }
    fn quarantine(allocation: Allocation) {
        allocation.context.shared.quarantine();
        let mut gate = lock(gate());
        gate.budget.quarantined += 1;
        // Each allocation has a pre-reserved Permit; retained.len <= MAX_OWNERS.
        gate.retained.push(allocation);
    }
    struct Owner(Option<Allocation>);
    impl Drop for Owner {
        fn drop(&mut self) {
            // Unexpected worker unwind is also fail-closed; do not let Box/lease
            // drop while the native object could still call back.
            if let Some(allocation) = self.0.take() {
                quarantine(allocation);
            }
        }
    }
    impl Owner {
        fn shutdown(mut self) -> Result<(), DecoderError> {
            let allocation = self.0.take().unwrap();
            allocation.context.shared.request_close();
            let codec = allocation.codec as *mut Codec;
            let stop = if allocation.started {
                check("OH_VideoDecoder_Stop", unsafe {
                    OH_VideoDecoder_Stop(codec)
                })
            } else {
                Ok(())
            };
            let code = if codec.is_null() {
                0
            } else {
                unsafe { OH_VideoDecoder_Destroy(codec) }
            };
            if code != 0 {
                quarantine(allocation);
                return Err(DecoderError::DestroyFailed { code });
            }
            allocation.context.wait_idle();
            if allocation.window != 0 {
                unsafe { OH_NativeWindow_DestroyNativeWindow(allocation.window as *mut Window) };
            }
            drop(allocation); // userdata, Surface lease and permit now safe.
            stop
        }
    }

    pub(super) fn open(
        config: DecoderConfig,
        lease: Arc<dyn SurfaceLease>,
    ) -> Result<SurfaceDecoder, DecoderError> {
        if lease.surface_id() == 0 {
            return Err(DecoderError::InvalidConfig);
        }
        let shared = Arc::new(Shared::new(config)?);
        let permit = Permit::acquire()?;
        let worker_shared = shared.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("rd-ohos-surface-decoder".into())
            .spawn(move || {
                let owner = Owner(Some(Allocation {
                    codec: 0,
                    window: 0,
                    started: false,
                    context: Box::new(Context {
                        shared: worker_shared.clone(),
                        active: AtomicUsize::new(0),
                        idle_lock: Mutex::new(()),
                        idle: Condvar::new(),
                    }),
                    _lease: lease,
                    _permit: permit,
                }));
                run(owner, config, worker_shared, ready_tx)
            })
            .map_err(|_| DecoderError::WorkerStartFailed)?;
        match ready_rx.recv() {
            Ok(Ok(codec_name)) => Ok(SurfaceDecoder {
                shared,
                worker: Some(worker),
                codec_name,
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(DecoderError::WorkerPanicked)
            }
        }
    }

    fn setup(owner: &mut Owner, config: DecoderConfig) -> Result<String, DecoderError> {
        let a = owner.0.as_mut().unwrap();
        let mime = match config.codec {
            VideoCodec::H264 => &b"video/avc\0"[..],
            VideoCodec::H265 => &b"video/hevc\0"[..],
        };
        let cap = unsafe { OH_AVCodec_GetCapabilityByCategory(mime.as_ptr(), false, 0) };
        if cap.is_null() || !unsafe { OH_AVCapability_IsHardware(cap) } {
            return Err(DecoderError::NoHardwareDecoder);
        }
        let name = unsafe { OH_AVCapability_GetName(cap) };
        if name.is_null() {
            return Err(DecoderError::NativeReturnedNull {
                api: "OH_AVCapability_GetName",
            });
        }
        let name = unsafe { CStr::from_ptr(name) }.to_owned();
        if name.as_bytes().is_empty() {
            return Err(DecoderError::NoHardwareDecoder);
        }
        let codec = unsafe { OH_VideoDecoder_CreateByName(name.as_ptr()) };
        a.codec = codec as usize;
        if codec.is_null() {
            return Err(DecoderError::NativeReturnedNull {
                api: "OH_VideoDecoder_CreateByName",
            });
        }
        let mut window = ptr::null_mut();
        let code = unsafe {
            OH_NativeWindow_CreateNativeWindowFromSurfaceId(a._lease.surface_id(), &mut window)
        };
        a.window = window as usize;
        check("OH_NativeWindow_CreateNativeWindowFromSurfaceId", code)?;
        if window.is_null() {
            return Err(DecoderError::NativeReturnedNull {
                api: "OH_NativeWindow_CreateNativeWindowFromSurfaceId",
            });
        }
        check("OH_VideoDecoder_RegisterCallback", unsafe {
            OH_VideoDecoder_RegisterCallback(
                codec,
                Callbacks {
                    error: on_error,
                    changed: on_changed,
                    input: on_input,
                    output: on_output,
                },
                (&*a.context) as *const Context as *mut c_void,
            )
        })?;
        let format =
            unsafe { OH_AVFormat_CreateVideoFormat(mime.as_ptr(), config.width, config.height) };
        if format.is_null() {
            return Err(DecoderError::NativeReturnedNull {
                api: "OH_AVFormat_CreateVideoFormat",
            });
        }
        let low_latency =
            unsafe { OH_AVFormat_SetIntValue(format, OH_MD_KEY_VIDEO_ENABLE_LOW_LATENCY, 1) };
        if !low_latency {
            unsafe { OH_AVFormat_Destroy(format) };
            return Err(DecoderError::Native {
                api: "OH_AVFormat_SetIntValue(low_latency)",
                code: -1,
            });
        }
        // Video variable refresh rate: the platform follows the stream's own
        // frame rate instead of the panel sitting at a rate this app guessed.
        // Best effort on purpose -- the feature is platform-dependent and a
        // platform without it must keep decoding normally rather than fail the
        // session over a hint. The frame rate key is set first because the
        // feature reads it.
        unsafe {
            OH_AVFormat_SetIntValue(format, OH_MD_KEY_FRAME_RATE, config.frame_rate as i32);
            OH_AVFormat_SetIntValue(format, OH_MD_KEY_VIDEO_DECODER_OUTPUT_ENABLE_VRR, 1);
        }
        let code = unsafe { OH_VideoDecoder_Configure(codec, format) };
        unsafe { OH_AVFormat_Destroy(format) };
        check("OH_VideoDecoder_Configure", code)?;
        check("OH_VideoDecoder_SetSurface", unsafe {
            OH_VideoDecoder_SetSurface(codec, window)
        })?;
        check("OH_VideoDecoder_Prepare", unsafe {
            OH_VideoDecoder_Prepare(codec)
        })?;
        check("OH_VideoDecoder_Start", unsafe {
            OH_VideoDecoder_Start(codec)
        })?;
        a.started = true;
        Ok(name.to_string_lossy().into_owned())
    }

    fn push(
        codec: *mut Codec,
        buffer: Buffer,
        unit: &AccessUnit,
        shared: &Shared,
    ) -> Result<(), DecoderError> {
        let native = buffer.address as *mut NativeBuffer;
        if native.is_null() {
            return Err(DecoderError::InvalidNativeBuffer);
        }
        if !unit.bytes.is_empty() {
            let capacity = unsafe { OH_AVBuffer_GetCapacity(native) };
            if capacity < 0 || unit.bytes.len() > capacity as usize {
                return Err(DecoderError::InputTooLarge {
                    size: unit.bytes.len(),
                    capacity,
                });
            }
            // ONLY compressed decoder input. Never call GetAddr on output.
            let address = unsafe { OH_AVBuffer_GetAddr(native) };
            if address.is_null() {
                return Err(DecoderError::InvalidNativeBuffer);
            }
            unsafe { ptr::copy_nonoverlapping(unit.bytes.as_ptr(), address, unit.bytes.len()) };
        }
        let flags = match unit.kind {
            AccessUnitKind::EndOfStream => EOS,
            AccessUnitKind::CodecConfig => CONFIG,
            AccessUnitKind::Frame { key: true } => SYNC,
            AccessUnitKind::Frame { key: false } => 0,
        };
        let attr = Attr {
            pts: unit.pts_us,
            size: unit.bytes.len() as i32,
            offset: 0,
            flags,
        };
        check("OH_AVBuffer_SetBufferAttr", unsafe {
            OH_AVBuffer_SetBufferAttr(native, &attr)
        })?;
        shared.returning_input(buffer.index);
        check("OH_VideoDecoder_PushInputBuffer", unsafe {
            OH_VideoDecoder_PushInputBuffer(codec, buffer.index)
        })
    }

    fn render(codec: *mut Codec, buffer: Buffer, shared: &Shared) -> Result<(), DecoderError> {
        let native = buffer.address as *mut NativeBuffer;
        if native.is_null() {
            return Err(DecoderError::InvalidNativeBuffer);
        }
        let mut attr = Attr::default();
        check("OH_AVBuffer_GetBufferAttr", unsafe {
            OH_AVBuffer_GetBufferAttr(native, &mut attr)
        })?;
        if attr.size < 0 || attr.offset < 0 {
            return Err(DecoderError::InvalidNativeBuffer);
        }
        // Surface output need not expose a CPU byte size. Classification uses
        // flags, never a pixel readback or size>0 as a fake presentation proof.
        let (frame, eos) = output_flags(attr.flags);
        shared.returning_output(buffer.index);
        if frame {
            // Timestamped rendering lets the surface coalesce multiple decoded
            // frames targeting one VSYNC and discard stale frames. The plain
            // RenderOutputBuffer API never drops for display-rate mismatch and
            // can fill the NativeWindow FIFO for seconds in interactive use.
            let render_timestamp_ns = monotonic_time_ns()?;
            check("OH_VideoDecoder_RenderOutputBufferAtTime", unsafe {
                OH_VideoDecoder_RenderOutputBufferAtTime(codec, buffer.index, render_timestamp_ns)
            })?;
        } else {
            check("OH_VideoDecoder_FreeOutputBuffer", unsafe {
                OH_VideoDecoder_FreeOutputBuffer(codec, buffer.index)
            })?;
        }
        // No retry-Free after a failed Render: consumption is not proven.
        shared.output(frame, eos);
        Ok(())
    }

    fn run(
        mut owner: Owner,
        config: DecoderConfig,
        shared: Arc<Shared>,
        ready: std::sync::mpsc::SyncSender<Result<String, DecoderError>>,
    ) -> Result<DecoderStats, DecoderError> {
        let initialized = setup(&mut owner, config).and_then(|name| match shared.stats().failure {
            Some(error) => Err(error),
            None => Ok(name),
        });
        let name = match initialized {
            Ok(name) => name,
            Err(error) => {
                shared.fail(error.clone());
                let result = owner.shutdown().err().unwrap_or(error);
                shared.fail(result.clone());
                shared.closed();
                let _ = ready.send(Err(result.clone()));
                return Err(result);
            }
        };
        if ready.send(Ok(name)).is_err() {
            shared.request_close();
        }
        let codec = owner.0.as_ref().unwrap().codec as *mut Codec;
        loop {
            let result = match shared.next() {
                Action::Stop => break,
                Action::Event(Event::Input(buffer)) => {
                    shared.input_available(buffer);
                    Ok(())
                }
                Action::Event(Event::Output(buffer)) => render(codec, buffer, &shared),
                Action::Event(Event::StreamChanged) => {
                    shared.stream_changed();
                    Ok(())
                }
                Action::Submit(buffer, unit) => {
                    let result = push(codec, buffer, &unit, &shared);
                    // Timed here because this is the last point where the unit
                    // still exists: the worker's queue carries indices, not
                    // payloads, so the arrival stamp cannot travel with it.
                    let elapsed = unit
                        .accepted_at
                        .map(|at| at.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
                    let queue = match (unit.accepted_at, unit.dequeued_at) {
                        (Some(arrived), Some(dequeued)) => Some(
                            dequeued
                                .saturating_duration_since(arrived)
                                .as_micros()
                                .min(u128::from(u64::MAX)) as u64,
                        ),
                        _ => None,
                    };
                    shared.submitted_at(unit.bytes.len(), result.is_ok(), elapsed, queue);
                    result
                }
            };
            if let Err(error) = result {
                shared.fail(error);
            }
        }
        let teardown = owner.shutdown();
        if let Err(error) = &teardown {
            shared.fail(error.clone());
        }
        shared.closed();
        teardown?;
        let stats = shared.stats();
        if let Some(error) = &stats.failure {
            return Err(error.clone());
        }
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::{engine::*, *};
    fn config() -> DecoderConfig {
        DecoderConfig {
            codec: VideoCodec::H265,
            width: 3840,
            height: 2160,
            // The variable refresh rate hint reads this, so the test config
            // carries a real rate rather than a zero.
            frame_rate: 60,
            max_queued_units: 1,
            max_queued_bytes: 16,
        }
    }
    fn packet() -> AccessUnit {
        AccessUnit {
            bytes: vec![1, 2, 3],
            pts_us: 1,
            kind: AccessUnitKind::Frame { key: false },
            // Unstamped: `submit` is what stamps a unit, exactly as on the real
            // path.
            accepted_at: None,
            dequeued_at: None,
        }
    }

    #[test]
    fn output_event_is_serviced_without_an_input_buffer() {
        let shared = Shared::new(config()).unwrap();
        shared.submit(packet()).unwrap();
        shared.post(Event::Output(Buffer {
            index: 7,
            address: 1,
        }));
        assert!(matches!(
            shared.next(),
            Action::Event(Event::Output(Buffer {
                index: 7,
                address: 1
            }))
        ));
        shared.returning_output(7);
        shared.output(true, false);
        shared.request_close();
        assert!(matches!(shared.next(), Action::Stop));
        shared.closed();
        assert_eq!(shared.stats().cancelled_units, 1);
        assert_eq!(shared.stats().render_submissions, 1);
    }

    #[test]
    fn in_progress_quota_and_synchronous_index_recycling_are_safe() {
        let shared = Shared::new(config()).unwrap();
        shared.submit(packet()).unwrap();
        shared.post(Event::Input(Buffer {
            index: 2,
            address: 1,
        }));
        if let Action::Event(Event::Input(buffer)) = shared.next() {
            shared.input_available(buffer);
        } else {
            panic!();
        }
        let (buffer, unit) = match shared.next() {
            Action::Submit(b, u) => (b, u),
            _ => panic!(),
        };
        assert_eq!(
            shared.submit(packet()).unwrap_err().reason,
            DecoderError::Backpressure
        );
        shared.returning_input(buffer.index);
        // Simulates metadata delivery while the SDK Push call is on the stack.
        shared.post(Event::Input(Buffer {
            index: 2,
            address: 1,
        }));
        assert!(shared.stats().failure.is_none());
        shared.submitted(unit.bytes.len(), true);
        shared.submit(packet()).unwrap();
    }

    #[test]
    fn callback_overflow_and_quarantine_are_bounded_and_fail_closed() {
        let shared = Shared::new(config()).unwrap();
        shared.stream_changed();
        for _ in 0..=MAX_CALLBACKS {
            shared.post(Event::StreamChanged);
        }
        assert_eq!(shared.stats().failure, Some(DecoderError::CallbackOverflow));
        assert!(matches!(shared.next(), Action::Stop));
        shared.fail(DecoderError::Closed);
        assert_eq!(shared.stats().failure, Some(DecoderError::CallbackOverflow));
        shared.quarantine();
        assert!(shared.stats().quarantined);
        let mut budget = OwnerBudget::default();
        for _ in 0..MAX_OWNERS {
            budget.reserve().unwrap();
        }
        assert_eq!(budget.reserve(), Err(DecoderError::OwnerLimit));
        budget.quarantined = 1;
        budget.live -= 1;
        assert_eq!(budget.reserve(), Err(DecoderError::QuarantinePresent));
    }

    #[test]
    fn eos_config_are_not_frames_and_eos_closes_submission_order() {
        assert_eq!(output_flags(0), (true, false));
        assert_eq!(output_flags(2 | 32), (true, false));
        assert_eq!(output_flags(8), (false, false));
        assert_eq!(output_flags(16), (false, false));
        assert_eq!(output_flags(1), (false, true));
        let shared = Shared::new(config()).unwrap();
        shared
            .submit(AccessUnit {
                bytes: Vec::new(),
                pts_us: 2,
                kind: AccessUnitKind::EndOfStream,
                accepted_at: None,
                dequeued_at: None,
            })
            .unwrap();
        let rejected = shared.submit(packet()).unwrap_err();
        assert_eq!(rejected.reason, DecoderError::EndOfStreamAlreadySubmitted);
        assert_eq!(rejected.unit.bytes, vec![1, 2, 3]);
    }
}
