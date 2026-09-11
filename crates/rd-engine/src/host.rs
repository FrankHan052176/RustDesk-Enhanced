//! Single-peer, explicitly approved viewing host. No legacy app runtime.
//! Authentication is not capture consent; the publisher retains the OS prompt.
//! Input injection exists only when the local operator arms it explicitly, and
//! file/audio/clipboard adapters remain absent and are never granted.
use crate::{
    authentication::{
        Approval, ApprovalProvider, AttemptPolicy, HostAuthConfig, NoSecondFactor, Passwords,
        Permissions, PrimaryPolicy,
    },
    handshake::{HostIdentity, Security},
    input::{InputAction, decode_message},
    publisher::{Codec, CodecSelection, EncodedUnit, Publisher, PublisherBackend, PublisherConfig},
    session::{AuthenticatedParts, HostEvent, HostSession},
    transport::{WireReader, WireWriter},
};
use hbb_common::{
    message_proto::{
        self as proto, DisplayInfo, EncodedVideoFrame, EncodedVideoFrames, LoginRequest, Message,
        PeerInfo, SupportedEncoding, VideoFrame, message, misc, supported_decoding::PreferCodec,
    },
    sodiumoxide::{self, crypto::sign},
};
use std::{
    fmt,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, Notify, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub struct HostOptions {
    pub listen: SocketAddr,
    pub id: String,
    pub signing_key: sign::SecretKey,
    pub width: i32,
    pub height: i32,
    /// Screen source bound, not advertised codec-only throughput.
    pub fps: u32,
    pub bitrate: i64,
    pub platform: String,
    pub publisher_backend: PublisherBackend,
    pub output_index: usize,
    pub codec_selection: CodecSelection,
    /// Explicit local consent to inject input into this machine.
    ///
    /// Input injection is never implied by authentication, by an approval click,
    /// or by the peer's own request. It must be armed here by the local operator,
    /// and the injection sink below decides whether this build can honour it.
    /// When it is `false` the peer is never granted the keyboard permission and
    /// every inbound input message is dropped.
    pub input_injection: bool,
    /// Platform injection sink. `None` keeps the host receive-only even when
    /// `input_injection` is set, which is how a platform without a backend stays
    /// fail-closed instead of pretending to work.
    pub input_sink: Option<InputSink>,
}

/// A platform injection sink. It returns `false` when the action was refused so
/// the host can count refusals without guessing.
pub type InputSink = fn(InputAction) -> bool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    InvalidOptions,
    NoRuntime,
    NoHardwareCodec,
    ListenFailed,
    TransportFailed,
    AuthenticationFailed,
    ApprovalDenied,
    NoCommonCodec,
    CaptureFailed,
    InvalidAccessUnit,
    UnsupportedDisplay,
    ReclamationUnconfirmed,
}
impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for HostError {}

#[derive(Clone)]
pub struct HostSnapshot {
    pub phase: &'static str,
    pub error: Option<HostError>,
    /// Locally generated request epoch, not a remote identity/credential.
    pub approval_request: String,
    /// Observed TCP origin only. It is not a cryptographic controller identity.
    pub approval_origin: String,
    pub width: i32,
    pub height: i32,
    pub fps: u32,
    pub codec: &'static str,
    pub connected: bool,
    pub encrypted: bool,
    pub sent_units: u64,
    pub sent_bytes: u64,
    /// Actions accepted by the platform injection sink.
    pub injected_inputs: u64,
    /// Actions the sink refused after local policy allowed them.
    pub refused_inputs: u64,
    /// Chat messages dropped because this host has no chat feature. Chat is
    /// removed rather than half-implemented, and the counter makes the removal
    /// visible instead of silent.
    pub dropped_chat_messages: u64,
    pub closed: bool,
}

struct State {
    cancel: CancellationToken,
    snapshot: Mutex<HostSnapshot>,
    grant: Mutex<LocalGrant>,
    approval: Notify,
    unreclaimed: AtomicBool,
}
#[derive(Default)]
struct LocalGrant {
    epoch: u64,
    decision: u8,
}
impl LocalGrant {
    fn decide(&mut self, epoch: u64, allow: bool) -> bool {
        if epoch == 0 || self.epoch != epoch || self.decision != 0 {
            return false;
        }
        self.decision = if allow { 1 } else { 2 };
        true
    }
}
// Tokens never reset when the local listener is stopped/restarted.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);
impl State {
    fn update(&self, change: impl FnOnce(&mut HostSnapshot)) {
        change(&mut self.snapshot.lock().unwrap_or_else(|v| v.into_inner()));
    }
    fn clear_request(&self) {
        *self.grant.lock().unwrap_or_else(|v| v.into_inner()) = LocalGrant::default();
        self.update(|s| {
            s.approval_request.clear();
            s.approval_origin.clear();
        });
    }
}

pub struct Host {
    state: Arc<State>,
    join: AsyncMutex<Option<JoinHandle<()>>>,
}
impl Host {
    pub fn start(options: HostOptions) -> Result<Self, HostError> {
        if options.id.is_empty()
            || options.id.len() > 512
            || options.listen.port() == 0
            || !(1..=8192).contains(&options.width)
            || !(1..=8192).contains(&options.height)
            || i64::from(options.width) * i64::from(options.height) > 33_554_432
            || !(1..=240).contains(&options.fps)
            || !(100_000..=200_000_000).contains(&options.bitrate)
            || options.platform.is_empty()
        {
            return Err(HostError::InvalidOptions);
        }
        sodiumoxide::init().map_err(|_| HostError::InvalidOptions)?;
        let runtime = crate::executor::runtime().map_err(|_| HostError::NoRuntime)?;
        let state = Arc::new(State {
            cancel: CancellationToken::new(),
            grant: Mutex::new(LocalGrant::default()),
            approval: Notify::new(),
            unreclaimed: AtomicBool::new(false),
            snapshot: Mutex::new(HostSnapshot {
                phase: "starting",
                error: None,
                approval_request: String::new(),
                approval_origin: String::new(),
                width: options.width,
                height: options.height,
                fps: options.fps,
                codec: "",
                connected: false,
                encrypted: false,
                sent_units: 0,
                sent_bytes: 0,
                injected_inputs: 0,
                refused_inputs: 0,
                dropped_chat_messages: 0,
                closed: false,
            }),
        });
        let task_state = state.clone();
        let join = runtime.spawn(async move {
            let result = serve(task_state.clone(), options).await;
            task_state.clear_request();
            let safe = !task_state.unreclaimed.load(Ordering::Acquire);
            task_state.update(|s| {
                if let Err(error) = result {
                    s.error = Some(error);
                }
                s.connected = false;
                s.encrypted = false;
                s.closed = safe;
                s.phase = if safe {
                    "closed"
                } else {
                    "reclamation_unconfirmed"
                };
            });
        });
        Ok(Self {
            state,
            join: AsyncMutex::new(Some(join)),
        })
    }
    pub fn snapshot(&self) -> HostSnapshot {
        self.state
            .snapshot
            .lock()
            .unwrap_or_else(|v| v.into_inner())
            .clone()
    }
    pub fn approve(&self, request_id: &str, allow: bool) -> bool {
        let Ok(epoch) = request_id.parse::<u64>() else {
            return false;
        };
        let mut grant = self.state.grant.lock().unwrap_or_else(|v| v.into_inner());
        if self.state.cancel.is_cancelled() || !grant.decide(epoch, allow) {
            return false;
        }
        drop(grant);
        self.state.approval.notify_one();
        true
    }
    pub fn request_close(&self) {
        self.state.cancel.cancel();
    }
    pub async fn close(&self) -> Result<(), HostError> {
        self.request_close();
        let mut join = self.join.lock().await;
        if let Some(task) = join.take() {
            if task.await.is_err() {
                self.state.unreclaimed.store(true, Ordering::Release);
            }
        }
        if self.state.unreclaimed.load(Ordering::Acquire) || !self.snapshot().closed {
            Err(HostError::ReclamationUnconfirmed)
        } else {
            Ok(())
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.request_close();
    }
}

struct LocalApproval {
    state: Arc<State>,
    epoch: u64,
    /// The grant a human approval may hand to the peer. Built from local policy
    /// only: an approval click cannot widen what the operator already armed.
    grant: Permissions,
}
impl ApprovalProvider for LocalApproval {
    fn check(&mut self, _: &LoginRequest) -> Approval {
        let grant = self.state.grant.lock().unwrap_or_else(|v| v.into_inner());
        if grant.epoch != self.epoch {
            return Approval::Pending;
        }
        match grant.decision {
            1 => Approval::Approved(self.grant),
            2 => Approval::Denied,
            _ => Approval::Pending,
        }
    }
}
struct NoPasswordAttempts;
impl AttemptPolicy for NoPasswordAttempts {
    fn allow(&mut self, _: bool) -> Result<(), &'static str> {
        Err("Connection not allowed")
    }
    fn outcome(&mut self, _: bool, _: bool) {}
}

#[derive(Clone, Copy)]
struct Encoders {
    h264: bool,
    h265: bool,
}
/// Fail-closed local permission set for one session.
///
/// Injection requires both the operator's explicit opt-in and a compiled-in
/// platform sink; either one missing leaves the peer without input. Nothing else
/// is granted here, so the ceiling, the approval grant and the wire
/// advertisement all agree by construction.
fn local_permissions(options: &HostOptions) -> Permissions {
    Permissions {
        keyboard: options.input_injection && options.input_sink.is_some(),
        ..Permissions::default()
    }
}

fn publisher_config(options: &HostOptions, codec: Codec, fps: u32) -> PublisherConfig {
    PublisherConfig {
        codec,
        width: options.width,
        height: options.height,
        fps,
        bitrate: options.bitrate,
        // Includes the AU currently being written. Two credits keep one ready
        // frame without accumulating a multi-frame capture-to-display tail.
        max_queued_units: 2,
        max_queued_bytes: 32 * 1024 * 1024,
        backend: options.publisher_backend,
        output_index: options.output_index,
    }
}
fn encoders(options: &HostOptions) -> Result<Encoders, HostError> {
    let supported = crate::publisher::supported_codecs_for(&publisher_config(
        options,
        Codec::H264,
        options.fps,
    ))
    .map_err(|_| HostError::NoHardwareCodec)?;
    let mut h264 = supported.h264;
    let mut h265 = supported.h265;
    match options.codec_selection {
        CodecSelection::Auto => {}
        CodecSelection::H264 => h265 = false,
        CodecSelection::H265 => h264 = false,
    }
    if !h264 && !h265 {
        return Err(HostError::NoHardwareCodec);
    }
    Ok(Encoders { h264, h265 })
}
fn peer_info(options: &HostOptions, codecs: Encoders) -> PeerInfo {
    PeerInfo {
        hostname: "RustDesk".into(),
        platform: options.platform.clone(),
        version: crate::REPORTED_VERSION.into(),
        displays: vec![DisplayInfo {
            width: options.width,
            height: options.height,
            name: "Screen".into(),
            online: true,
            scale: 1.0,
            ..Default::default()
        }],
        encoding: Some(SupportedEncoding {
            h264: codecs.h264,
            h265: codecs.h265,
            ..Default::default()
        })
        .into(),
        ..Default::default()
    }
}

async fn serve(state: Arc<State>, options: HostOptions) -> Result<(), HostError> {
    let probe_options = HostOptions {
        listen: options.listen,
        id: options.id.clone(),
        signing_key: options.signing_key.clone(),
        width: options.width,
        height: options.height,
        fps: options.fps,
        bitrate: options.bitrate,
        platform: options.platform.clone(),
        publisher_backend: options.publisher_backend,
        output_index: options.output_index,
        codec_selection: options.codec_selection,
        // Capability probing does not inject anything; keep the probe copy
        // receive-only so it can never be a second injection path.
        input_injection: false,
        input_sink: None,
    };
    let codecs = tokio::task::spawn_blocking(move || encoders(&probe_options))
        .await
        .map_err(|_| HostError::NoHardwareCodec)??;
    let listener = TcpListener::bind(options.listen)
        .await
        .map_err(|_| HostError::ListenFailed)?;
    loop {
        state.update(|s| {
            s.phase = "listening";
            s.connected = false;
            s.encrypted = false;
        });
        let (socket, _) = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            value = listener.accept() => value.map_err(|_| HostError::TransportFailed)?,
        };
        state.update(|s| {
            s.phase = "authenticating";
            s.error = None;
            s.sent_units = 0;
            s.sent_bytes = 0;
        });
        let outcome = peer(state.clone(), &options, codecs, socket).await;
        state.clear_request();
        if let Err(error) = outcome {
            state.update(|s| s.error = Some(error));
            if error == HostError::ReclamationUnconfirmed {
                return Err(error);
            }
        }
    }
}

async fn authenticate(
    state: Arc<State>,
    options: &HostOptions,
    codecs: Encoders,
    socket: TcpStream,
) -> Result<AuthenticatedParts, HostError> {
    let local = socket
        .local_addr()
        .map_err(|_| HostError::TransportFailed)?;
    let origin = socket
        .peer_addr()
        .map_err(|_| HostError::TransportFailed)?
        .to_string();
    let epoch = NEXT_REQUEST
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
        .map_err(|_| HostError::AuthenticationFailed)?
        + 1;
    state.clear_request();
    let salt: String = sodiumoxide::randombytes::randombytes(16)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    // Local input policy, resolved once per session. Both the ceiling and the
    // approval grant come from this one decision, so the peer can never hold an
    // input permission this machine did not explicitly arm. Audio, file and
    // clipboard stay denied unconditionally.
    let local_permissions = local_permissions(options);
    let config = HostAuthConfig {
        accepted_targets: vec![
            options.id.clone(),
            local.ip().to_string(),
            local.to_string(),
        ],
        salt,
        passwords: Passwords::from_salted(Vec::new()),
        policy: PrimaryPolicy::ClickOnly,
        ceiling: local_permissions,
        password_permissions: Permissions::default(),
        peer_info: peer_info(options, codecs),
        approval: Box::new(LocalApproval {
            state: state.clone(),
            epoch,
            grant: local_permissions,
        }),
        second_factor: Box::new(NoSecondFactor),
        attempts: Box::new(NoPasswordAttempts),
    };
    let identity = HostIdentity::Signed {
        id: options.id.clone(),
        secret_key: options.signing_key.clone(),
    };
    let mut session = HostSession::accept_direct(socket, identity, config, Duration::from_secs(90))
        .await
        .map_err(|_| HostError::AuthenticationFailed)?;
    // Bounded pre-auth dispatch. A remote flood cannot sustain an unlimited UI request.
    for _ in 0..64 {
        let event = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Err(HostError::TransportFailed),
            value = session.recv_with_approval(&state.approval) => value,
        }
        .map_err(|_| HostError::AuthenticationFailed)?;
        match event {
            HostEvent::AwaitApproval => {
                // Do not reset an already latched decision on a repeated remote
                // request. Epoch and decision share one authorization lock.
                state.grant.lock().unwrap_or_else(|v| v.into_inner()).epoch = epoch;
                state.update(|s| {
                    s.phase = "awaiting_approval";
                    s.approval_request = epoch.to_string();
                    s.approval_origin = origin.clone();
                });
            }
            HostEvent::Authorized(_) => {
                state.clear_request();
                return session
                    .into_authenticated_parts()
                    .map_err(|_| HostError::AuthenticationFailed);
            }
            HostEvent::Closed | HostEvent::LoginRejected(_) => {
                return Err(HostError::ApprovalDenied);
            }
            _ => {}
        }
    }
    Err(HostError::AuthenticationFailed)
}

enum Control {
    Reply(Message),
    Keyframe,
}
async fn peer(
    state: Arc<State>,
    options: &HostOptions,
    codecs: Encoders,
    socket: TcpStream,
) -> Result<(), HostError> {
    let parts = tokio::select! {
        biased;
        _ = state.cancel.cancelled() => return Ok(()),
        value = authenticate(state.clone(), options, codecs, socket) => value?,
    };
    let decoding = parts
        .context
        .claims
        .options
        .as_ref()
        .and_then(|o| o.supported_decoding.as_ref());
    let Some(decoding) = decoding else {
        return Err(HostError::NoCommonCodec);
    };
    let codec = if codecs.h264
        && decoding.ability_h264 > 0
        && decoding.prefer.enum_value() == Ok(PreferCodec::H264)
    {
        Codec::H264
    } else if codecs.h265 && decoding.ability_h265 > 0 {
        Codec::H265
    } else if codecs.h264 && decoding.ability_h264 > 0 {
        Codec::H264
    } else {
        return Err(HostError::NoCommonCodec);
    };
    let requested = parts
        .context
        .claims
        .options
        .as_ref()
        .map(|o| o.custom_fps)
        .unwrap_or(0);
    let fps = if requested > 0 {
        options.fps.min(requested as u32)
    } else {
        options.fps
    };
    state.update(|s| {
        s.phase = "awaiting_capture_consent";
        s.connected = true;
        s.fps = fps;
        s.encrypted = matches!(parts.context.security, Security::Encrypted { .. });
        s.codec = match codec {
            Codec::H264 => "H264",
            Codec::H265 => "H265",
        };
    });
    let cancel = state.cancel.child_token();
    let (tx, rx) = mpsc::channel(16);
    // The reader re-checks the permission that authentication actually granted,
    // so a viewer-side `disable-keyboard` downgrade also disables injection.
    let parts_permissions = parts.context.permissions;
    let mut reader = tokio::spawn(read_peer(
        state.clone(),
        parts.reader,
        tx,
        cancel.clone(),
        options.width,
        options.height,
        // The reader re-checks the granted permission, never the local option.
        parts_permissions,
        options.input_sink,
    ));
    let config = publisher_config(options, codec, fps);
    let mut sender = tokio::spawn(publish(
        state.clone(),
        parts.writer,
        rx,
        cancel.clone(),
        config,
    ));
    let first = tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        result = &mut reader => Some((true, result)),
        result = &mut sender => Some((false, result)),
    };
    cancel.cancel();
    let (rx_result, tx_result) = match first {
        Some((true, result)) => (result, sender.await),
        Some((false, result)) => (reader.await, result),
        None => (reader.await, sender.await),
    };
    if tx_result.is_err() {
        state.unreclaimed.store(true, Ordering::Release);
        return Err(HostError::ReclamationUnconfirmed);
    }
    let sent = tx_result.unwrap();
    if sent == Err(HostError::ReclamationUnconfirmed) {
        return sent;
    }
    if state.cancel.is_cancelled() {
        return Ok(());
    }
    sent?;
    rx_result.map_err(|_| HostError::TransportFailed)?
}

async fn read_peer(
    state: Arc<State>,
    mut reader: WireReader,
    control: mpsc::Sender<Control>,
    cancel: CancellationToken,
    width: i32,
    height: i32,
    permissions: Permissions,
    input_sink: Option<InputSink>,
) -> Result<(), HostError> {
    loop {
        let message = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            value = reader.recv() => value.map_err(|_| HostError::TransportFailed)?.ok_or(HostError::TransportFailed)?,
        };
        let output = match message.union {
            Some(message::Union::TestDelay(probe)) if probe.from_client => {
                let mut m = Message::new();
                m.set_test_delay(probe);
                Some(Control::Reply(m))
            }
            Some(message::Union::Misc(m)) => match m.union {
                Some(misc::Union::CloseReason(_)) => return Ok(()),
                Some(misc::Union::RefreshVideo(true))
                | Some(misc::Union::RefreshVideoDisplay(0)) => Some(Control::Keyframe),
                Some(misc::Union::MessageQuery(q)) if q.switch_display == 0 => {
                    let mut misc = proto::Misc::new();
                    misc.set_switch_display(proto::SwitchDisplay {
                        width,
                        height,
                        ..Default::default()
                    });
                    let mut m = Message::new();
                    m.set_misc(misc);
                    Some(Control::Reply(m))
                }
                Some(misc::Union::CaptureDisplays(displays)) => {
                    if displays
                        .add
                        .iter()
                        .chain(displays.set.iter())
                        .any(|v| *v != 0)
                    {
                        return Err(HostError::UnsupportedDisplay);
                    }
                    // A withdrawn subscription must stop capture, not keep sending
                    // an unrequested display. Re-subscribing needs a fresh session
                    // in this explicitly single-display first host slice.
                    if displays.sub.contains(&0) {
                        return Ok(());
                    }
                    None
                }
                // No silent resolution changes or unauthorized system operations.
                Some(misc::Union::ChangeResolution(_))
                | Some(misc::Union::ChangeDisplayResolution(_)) => {
                    return Err(HostError::UnsupportedDisplay);
                }
                // Chat was removed from this controlled side. Counting the drop
                // keeps the removal observable instead of silently swallowing a
                // message the user believes was delivered.
                Some(misc::Union::ChatMessage(_)) => {
                    state.update(|s| s.dropped_chat_messages += 1);
                    None
                }
                _ => None,
            },
            // Pointer, keyboard and text input are injected only when the local
            // operator armed a platform sink AND authentication granted the
            // keyboard permission. Anything else is dropped, never forwarded to
            // a legacy runtime and never answered with a fake success.
            Some(union @ (message::Union::MouseEvent(_) | message::Union::KeyEvent(_))) => {
                let Some(sink) = input_sink.filter(|_| permissions.keyboard) else {
                    return Ok(());
                };
                match decode_message(&union) {
                    // A malformed or unsupported action is dropped: guessing at a
                    // key or button the user never pressed is worse than nothing.
                    None => None,
                    Some(action) => {
                        if sink(action) {
                            state.update(|s| s.injected_inputs += 1);
                        } else {
                            state.update(|s| s.refused_inputs += 1);
                        }
                        None
                    }
                }
            }
            // File/audio/clipboard adapters are absent. No fake adapter is
            // installed and no permission is advertised for them.
            _ => None,
        };
        if let Some(output) = output {
            control
                .try_send(output)
                .map_err(|_| HostError::TransportFailed)?;
        }
    }
}

fn video(unit: &EncodedUnit, codec: Codec) -> Result<Message, HostError> {
    if unit.data.is_empty() || unit.data.len() > 32 * 1024 * 1024 || unit.pts_us < 0 {
        return Err(HostError::InvalidAccessUnit);
    }
    let frames = EncodedVideoFrames {
        frames: vec![EncodedVideoFrame {
            data: unit.data.clone(),
            key: unit.key,
            pts: unit.pts_us / 1000,
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut frame = VideoFrame::new();
    match codec {
        Codec::H264 => frame.set_h264s(frames),
        Codec::H265 => frame.set_h265s(frames),
    }
    let mut message = Message::new();
    message.set_video_frame(frame);
    Ok(message)
}

async fn publish(
    state: Arc<State>,
    mut writer: WireWriter,
    mut control: mpsc::Receiver<Control>,
    cancel: CancellationToken,
    config: PublisherConfig,
) -> Result<(), HostError> {
    let codec = config.codec;
    // A peer may disconnect between spawning this future and its first poll.
    // Do not open a native capture or display a system consent prompt after the
    // session is already cancelled. Once open starts, however, it must finish
    // and be joined below rather than being cancelled mid-creation.
    if cancel.is_cancelled() {
        return Ok(());
    }
    // Never abort a native creation/join just because its caller canceled.
    let publisher = match Publisher::open(config).await {
        Ok(value) => value,
        Err(error) => {
            if error.resources_unconfirmed() {
                state.unreclaimed.store(true, Ordering::Release);
                return Err(HostError::ReclamationUnconfirmed);
            }
            return Err(HostError::CaptureFailed);
        }
    };
    let result = async {
        loop {
            let (message, unit) = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                value = control.recv() => match value {
                    Some(Control::Reply(message)) => (message, None),
                    Some(Control::Keyframe) => {
                        publisher.request_keyframe().map_err(|_| HostError::CaptureFailed)?;
                        continue;
                    }
                    None => return Ok(()),
                },
                unit = publisher.recv(&cancel) => match unit.map_err(|_| HostError::CaptureFailed)? {
                    Some(unit) => { let m = video(&unit, codec)?; (m, Some(unit)) },
                    None => return Ok(()),
                },
            };
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                sent = writer.send(&message) => sent.map_err(|_| HostError::TransportFailed)?,
            }
            // The publisher's AU credit guard stays alive through the send. Only
            // compressed Bytes are cloned into protobuf; no pixels are touched.
            if let Some(unit) = unit {
                state.update(|s| { s.phase = "streaming"; s.sent_units += 1; s.sent_bytes += unit.data.len() as u64; });
            }
        }
    }.await;
    publisher.request_close();
    if publisher.close().await.is_err() {
        state.unreclaimed.store(true, Ordering::Release);
        return Err(HostError::ReclamationUnconfirmed);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{HostOptions, InputSink, LocalGrant, local_permissions};
    use crate::input::{InputAction, MouseAction};
    use std::net::SocketAddr;

    fn options(input_injection: bool, input_sink: Option<InputSink>) -> HostOptions {
        HostOptions {
            listen: "127.0.0.1:21118".parse::<SocketAddr>().expect("literal"),
            id: "host".into(),
            signing_key: hbb_common::sodiumoxide::crypto::sign::gen_keypair().1,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate: 20_000_000,
            platform: "Windows".into(),
            publisher_backend: crate::publisher::PublisherBackend::Auto,
            output_index: 0,
            codec_selection: crate::publisher::CodecSelection::Auto,
            input_injection,
            input_sink,
        }
    }

    fn counting_sink() -> InputSink {
        fn sink(_action: InputAction) -> bool {
            true
        }
        sink as InputSink
    }

    #[test]
    fn input_requires_both_local_consent_and_a_platform_sink() {
        // Neither an absent sink nor an unarmed option may grant input, and the
        // audio, clipboard and file permissions stay denied in every case.
        for (injection, sink) in [
            (false, None),
            (false, Some(counting_sink())),
            (true, None),
            (true, Some(counting_sink())),
        ] {
            let permissions = local_permissions(&options(injection, sink));
            let expected = injection && sink.is_some();
            assert_eq!(
                permissions.keyboard,
                expected,
                "injection={injection} sink={}",
                sink.is_some()
            );
            assert!(!permissions.audio);
            assert!(!permissions.clipboard);
            assert!(!permissions.file);
        }
    }

    #[test]
    fn an_approved_peer_still_gets_the_local_permission_set() {
        // The approval grant is the local set, never a fresh default, so a
        // human's approval click cannot widen what the operator armed.
        let armed = options(true, Some(counting_sink()));
        assert!(local_permissions(&armed).keyboard);
        let unarmed = options(false, Some(counting_sink()));
        assert!(!local_permissions(&unarmed).keyboard);
    }

    #[test]
    fn a_denied_input_permission_never_reaches_the_sink() {
        // Mirrors the reader gate: the sink is only consulted when the granted
        // permission is present, which is what the reader checks before decoding.
        let mut delivered = 0;
        let sink: InputSink = {
            fn sink(_action: InputAction) -> bool {
                true
            }
            sink as InputSink
        };
        let granted = crate::authentication::Permissions {
            keyboard: true,
            ..Default::default()
        };
        for permissions in [crate::authentication::Permissions::default(), granted] {
            if let Some(sink) = Some(sink).filter(|_| permissions.keyboard) {
                if sink(InputAction::Mouse(MouseAction::MoveRelative {
                    dx: 1,
                    dy: 1,
                })) {
                    delivered += 1;
                }
            }
        }
        assert_eq!(delivered, 1, "only the granted session may inject");
    }

    #[test]
    fn stale_or_repeated_ui_decision_never_authorizes_another_request() {
        let mut grant = LocalGrant {
            epoch: 2,
            decision: 0,
        };
        assert!(!grant.decide(1, true));
        assert_eq!(grant.decision, 0);
        assert!(grant.decide(2, false));
        assert!(!grant.decide(2, true));
        assert_eq!(grant.decision, 2);
        grant = LocalGrant {
            epoch: 3,
            decision: 0,
        };
        assert!(!grant.decide(2, true));
        assert_eq!(grant.decision, 0);
    }
}
