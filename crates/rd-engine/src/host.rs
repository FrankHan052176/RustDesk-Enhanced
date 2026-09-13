//! Single-peer, explicitly approved viewing host. No legacy app runtime.
//! Authentication is not capture consent; the publisher retains the OS prompt.
//! Input injection exists only when the local operator arms it explicitly, and
//! file/audio/clipboard adapters remain absent and are never granted.
use crate::{
    authentication::{
        Approval, ApprovalProvider, AttemptPolicy, HostAuthConfig, NoSecondFactor, Passwords,
        Permissions, PrimaryPolicy, salted_password,
    },
    handshake::{HostIdentity, Security},
    input::{InputAction, decode_message},
    publisher::{Codec, CodecSelection, EncodedUnit, Publisher, PublisherBackend, PublisherConfig},
    session::{AuthenticatedParts, HostEvent, HostSession},
    transport::{WireReader, WireWriter},
};
use hbb_common::{
    config::{Config, decode_permanent_password_h1_from_storage},
    message_proto::{
        self as proto, DisplayInfo, EncodedVideoFrame, EncodedVideoFrames, LoginRequest, Message,
        PeerInfo, SupportedEncoding, VideoFrame, message, misc, supported_decoding::PreferCodec,
    },
    password_security::{self, ApproveMode},
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
    /// Whether `bitrate` is a starting point to adapt from rather than a fixed
    /// ceiling.
    pub bitrate_auto: bool,
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
    /// Input records dropped because this session has no injection permission or
    /// no platform sink. The session stays up: the original host skips the
    /// injection and keeps the connection, and a close here turns a controller's
    /// mouse move into a reconnect and a fresh password prompt.
    pub dropped_inputs: u64,
    /// Display requests this single-display host cannot honour. Counted, not
    /// fatal, for the same reason.
    pub ignored_display_requests: u64,
    /// How often the platform recorder had to be reopened during this session.
    /// A recorder restart stays inside the session, so this counter is the only
    /// trace of it in the host's own state.
    pub capture_restarts: u64,
    /// Whether the peer currently subscribes to this host's display. An
    /// unsubscribe stops the video stream without ending the session, which is
    /// what the original host does when a controller switches displays.
    pub video_subscribed: bool,
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
        // The one-time password is minted when the host starts, so the operator
        // can read it -- and rotate it from the UI -- before anyone connects.
        // Minting it inside the first login instead handed that client a
        // challenge it had no way to answer, and left the screen saying
        // "waiting to be generated" for as long as nobody connected.
        password_security::update_temporary_password();
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
                dropped_inputs: 0,
                ignored_display_requests: 0,
                capture_restarts: 0,
                video_subscribed: true,
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
/// The passwords this host accepts, from the sources the original host uses.
///
/// The one-time password is plaintext in memory, so it is salted with the salt
/// this session announces, which is what a client's first hash is built against.
/// The permanent password is stored as a finished first hash tied to its own
/// preset salt, so accepting it means announcing that salt instead; that storage
/// is not wired in yet.
fn host_passwords(
    salt: &str,
    temporary: &str,
    permanent: Option<[u8; 32]>,
    allow_permanent: bool,
    allow_temporary: bool,
) -> Vec<[u8; 32]> {
    let mut values = Vec::new();
    // The stored hash is already the first half of the chain, tied to the salt
    // this session announces.
    if allow_permanent {
        if let Some(h1) = permanent {
            values.push(h1);
        }
    }
    if allow_temporary && !temporary.is_empty() {
        values.push(salted_password(temporary.as_bytes(), salt));
    }
    values
}

/// The stored permanent password's first hash, when the operator set one.
fn permanent_password_h1() -> Option<[u8; 32]> {
    let (storage, _) = Config::get_local_permanent_password_storage_and_salt();
    decode_permanent_password_h1_from_storage(&storage)
}

/// The approval policy the operator configured, in this host's vocabulary.
fn host_policy(mode: ApproveMode) -> PrimaryPolicy {
    match mode {
        ApproveMode::Password => PrimaryPolicy::PasswordOnly,
        ApproveMode::Click => PrimaryPolicy::ClickOnly,
        ApproveMode::Both => PrimaryPolicy::PasswordOrClick,
    }
}

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
        // The CLI's `--bitrate auto` (and its default) is the sentinel: it seeds
        // the starting ceiling, and the session then follows the picture instead
        // of holding that value.
        auto_bitrate: options.bitrate_auto,
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
        bitrate_auto: options.bitrate_auto,
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
                // Quarantine: native ownership could not be confirmed, so no new
                // session is accepted. The phase says so instead of leaving a
                // listener that answers and then drops every peer, because a
                // local operator restarts a host that reports stopped.
                state.update(|s| s.phase = "stopped");
                return Err(error);
            }
        }
    }
}

/// Every spelling of this machine that a peer may put in a login request's
/// `username`.
///
/// The login check is an exact string match, so the list has to carry the forms
/// a client actually sends. The original client's direct connection sends the
/// address it was given, not this socket's local address -- and with
/// `--listen 0.0.0.0:21118` that local address is `0.0.0.0:21118`, which no
/// client ever sends. Listing only the configured id and the socket's own
/// spelling rejected every real peer with "Offline" before it could present a
/// password.
fn accepted_targets(options: &HostOptions, local: SocketAddr) -> Vec<String> {
    let mut targets = vec![
        options.id.clone(),
        // The advertised address, with and without its port: an operator types
        // whichever form, and the client echoes it back.
        local.ip().to_string(),
        local.to_string(),
        options.listen.ip().to_string(),
        options.listen.to_string(),
    ];
    for spelling in [&options.id, &local.to_string(), &options.listen.to_string()] {
        // Only a single colon is a host:port split; an IPv6 literal keeps its
        // brackets.
        if let Some((host, _)) = spelling.rsplit_once(':') {
            if !host.contains(':') {
                targets.push(host.to_owned());
            }
        }
    }
    // A wildcard bind address is never an identity a peer sends, in either its
    // bare or its host:port spelling.
    targets.retain(|target| {
        if target.is_empty() {
            return false;
        }
        let host = match target.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
            _ => target.as_str(),
        };
        host != "0.0.0.0" && host != "::" && host != "[::]"
    });
    targets.sort();
    targets.dedup();
    targets
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
    // One line per accepted connection, with the epoch that appears in every
    // later approval line. Without it a session that stalls before approval
    // leaves no trace of having reached the host at all.
    eprintln!("auth_begin epoch={epoch} origin={origin}");
    // A client's first hash is built against the salt this session announces, so
    // a stored permanent password only matches when that salt is the one it was
    // hashed with. Without one, a fresh random salt is announced.
    let stored_salt = Config::get_effective_permanent_password_salt();
    let salt: String = if stored_salt.is_empty() {
        sodiumoxide::randombytes::randombytes(16)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    } else {
        stored_salt
    };
    // Local input policy, resolved once per session. Both the ceiling and the
    // approval grant come from this one decision, so the peer can never hold an
    // input permission this machine did not explicitly arm. Audio, file and
    // clipboard stay denied unconditionally.
    let local_permissions = local_permissions(options);
    // The one-time password is plaintext in memory, so this session's salt is the
    // one a client's first hash is built against. `Host::start` mints it, so this
    // is only the fallback for a host whose password was cleared while running.
    if password_security::temporary_password().is_empty() {
        password_security::update_temporary_password();
    }
    let passwords = host_passwords(
        &salt,
        &password_security::temporary_password(),
        permanent_password_h1(),
        password_security::permanent_enabled(),
        password_security::temporary_enabled(),
    );
    let config = HostAuthConfig {
        accepted_targets: accepted_targets(options, local),
        salt,
        passwords: Passwords::from_salted(passwords),
        policy: host_policy(password_security::approve_mode()),
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
    // The original host presents its signed identity only when it actually holds
    // an hbbs-issued signing key pair; a listener that is not registered with a
    // rendezvous server has none, and a peer could not verify one signed by a key
    // it has never seen. It sends nothing and lets the password challenge be the
    // first record, exactly as `on_open` writes it.
    //
    // Sending an unverifiable identity instead is not harmless: a direct peer
    // reads one record, ignores the identity it cannot check, and then waits for
    // the challenge -- while this side waits for a record that the peer has no
    // reason to send. The connection then idles until a timeout closes it, which
    // the peer reports as a reset by the peer.
    let identity = HostIdentity::LegacyPlain;
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
        .map_err(|error| {
            // The phase alone cannot say whether a stalled session timed out,
            // closed, or failed to parse; the reason is what names the step.
            eprintln!("auth_recv_failed epoch={epoch} origin={origin} error={error}");
            HostError::AuthenticationFailed
        })?;
        let event_name = match &event {
            HostEvent::Progress => "progress",
            HostEvent::AwaitApproval => "awaiting_approval",
            HostEvent::LoginRejected(reason) => reason,
            HostEvent::Authorized(_) => "authorized",
            HostEvent::Unsupported => "unsupported",
            HostEvent::Closed => "closed",
        };
        eprintln!("auth_event epoch={epoch} origin={origin} event={event_name}");
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
                let parts = session.into_authenticated_parts().map_err(|_| {
                    eprintln!("auth_parts_failed epoch={epoch} origin={origin}");
                    HostError::AuthenticationFailed
                })?;
                eprintln!("auth_authorized epoch={epoch} origin={origin}");
                return Ok(parts);
            }
            HostEvent::Closed | HostEvent::LoginRejected(_) => {
                return Err(HostError::ApprovalDenied);
            }
            _ => {}
        }
    }
    // The bounded loop ran out: 64 records arrived and none of them completed
    // authentication. Naming that is the difference between a stalled session
    // and a peer that keeps sending.
    eprintln!("auth_exhausted epoch={epoch} origin={origin} records=64");
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

/// Whether the peer is currently subscribed to this host's display.
fn subscribed(state: &State) -> bool {
    state
        .snapshot
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .video_subscribed
}

/// Why a peer record was dropped instead of answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dropped {
    /// This host has no chat feature.
    Chat,
    /// A display operation this single-display host cannot perform.
    Display,
}

/// What one peer record means for the session.
///
/// The classification mirrors the original host's dispositions: only a close
/// request ends the connection. Input without a permission, display operations
/// for another display and features this host does not have are dropped, because
/// closing the session for them turns a controller's ordinary action into a
/// reconnect and a fresh password prompt on the far side.
#[derive(Debug, Clone)]
enum Disposition {
    /// Nothing to do; the session continues.
    None,
    /// A record that needs an answer on the wire.
    Reply(Message),
    /// Ask the encoder for a keyframe.
    Keyframe,
    /// Pointer, keyboard or text input.
    Input,
    /// The peer's subscription to this host's display changed.
    Subscribed(bool),
    /// Dropped, with the reason counted.
    Dropped(Dropped),
    /// The peer asked to end the session.
    Close,
}

fn classify(message: &Message, width: i32, height: i32) -> Disposition {
    match &message.union {
        Some(message::Union::TestDelay(probe)) if probe.from_client => {
            let mut reply = Message::new();
            reply.set_test_delay(probe.clone());
            Disposition::Reply(reply)
        }
        Some(message::Union::Misc(m)) => match &m.union {
            Some(misc::Union::CloseReason(_)) => Disposition::Close,
            Some(misc::Union::RefreshVideo(true)) | Some(misc::Union::RefreshVideoDisplay(0)) => {
                Disposition::Keyframe
            }
            Some(misc::Union::MessageQuery(q)) if q.switch_display == 0 => {
                let mut misc = proto::Misc::new();
                misc.set_switch_display(proto::SwitchDisplay {
                    width,
                    height,
                    ..Default::default()
                });
                let mut reply = Message::new();
                reply.set_misc(misc);
                Disposition::Reply(reply)
            }
            Some(misc::Union::CaptureDisplays(displays)) => {
                // Another display cannot be served by this single-display host,
                // and a subscription change for this one only starts or stops the
                // stream. Neither is a reason to close the connection.
                if displays
                    .add
                    .iter()
                    .chain(displays.set.iter())
                    .any(|display| *display != 0)
                {
                    return Disposition::Dropped(Dropped::Display);
                }
                if displays.sub.contains(&0) {
                    return Disposition::Subscribed(false);
                }
                if displays.add.contains(&0) || displays.set.contains(&0) {
                    return Disposition::Subscribed(true);
                }
                Disposition::None
            }
            // This host never changes the local screen's resolution, and it says
            // so by ignoring the request rather than by dropping the session.
            Some(misc::Union::ChangeResolution(_))
            | Some(misc::Union::ChangeDisplayResolution(_)) => {
                Disposition::Dropped(Dropped::Display)
            }
            Some(misc::Union::ChatMessage(_)) => Disposition::Dropped(Dropped::Chat),
            _ => Disposition::None,
        },
        Some(
            message::Union::MouseEvent(_)
            | message::Union::KeyEvent(_)
            | message::Union::PointerDeviceEvent(_),
        ) => Disposition::Input,
        _ => Disposition::None,
    }
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
        let output = match classify(&message, width, height) {
            Disposition::Close => {
                // The peer asked to end the session; that is the one record that
                // closes it.
                return Ok(());
            }
            Disposition::Reply(reply) => Some(Control::Reply(reply)),
            Disposition::Keyframe => Some(Control::Keyframe),
            Disposition::Subscribed(subscribed) => {
                state.update(|s| s.video_subscribed = subscribed);
                None
            }
            Disposition::Input => {
                // Injection needs both the granted permission and a platform
                // sink. Without them the record is dropped and the session stays
                // up, exactly as the original host skips an injection it is not
                // allowed to perform.
                let sink = input_sink.filter(|_| permissions.keyboard);
                match (sink, message.union.as_ref().and_then(decode_message)) {
                    // A malformed or unsupported action is dropped: guessing at a
                    // key or button the user never pressed is worse than nothing.
                    (Some(sink), Some(action)) => {
                        if sink(action) {
                            state.update(|s| s.injected_inputs += 1);
                        } else {
                            state.update(|s| s.refused_inputs += 1);
                        }
                        None
                    }
                    _ => {
                        state.update(|s| s.dropped_inputs += 1);
                        None
                    }
                }
            }
            Disposition::Dropped(reason) => {
                state.update(|s| match reason {
                    Dropped::Chat => s.dropped_chat_messages += 1,
                    Dropped::Display => s.ignored_display_requests += 1,
                });
                None
            }
            Disposition::None => None,
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
    // A capture that fails or ends must not end the session: the peer reads a
    // closed connection as a dropped session and reconnects with a fresh
    // password prompt, while the failure belongs to the platform recorder and
    // not to the peer. Reclamation stays strict, and a publisher whose native
    // owner cannot be confirmed still quarantines the host.
    let mut restarts: u32 = 0;
    loop {
        let publisher = match Publisher::open(config).await {
            Ok(value) => value,
            Err(error) => {
                if error.resources_unconfirmed() {
                    state.unreclaimed.store(true, Ordering::Release);
                    return Err(HostError::ReclamationUnconfirmed);
                }
                state.update(|s| s.capture_restarts += 1);
                if !backoff(&cancel, restarts).await {
                    return Ok(());
                }
                restarts += 1;
                continue;
            }
        };
        let outcome = stream(
            &state,
            &mut writer,
            &mut control,
            &cancel,
            &publisher,
            codec,
        )
        .await;
        publisher.request_close();
        if publisher.close().await.is_err() {
            state.unreclaimed.store(true, Ordering::Release);
            return Err(HostError::ReclamationUnconfirmed);
        }
        match outcome {
            StreamEnd::Cancelled | StreamEnd::PeerGone => return Ok(()),
            StreamEnd::Transport => return Err(HostError::TransportFailed),
            StreamEnd::Capture => {
                state.update(|s| s.capture_restarts += 1);
                if !backoff(&cancel, restarts).await {
                    return Ok(());
                }
                restarts += 1;
            }
        }
    }
}

/// Why a capture stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamEnd {
    /// The session was cancelled locally.
    Cancelled,
    /// The peer-facing side of the session is gone.
    PeerGone,
    /// The socket to the peer failed.
    Transport,
    /// The platform recorder failed or stopped. Reopenable.
    Capture,
}

/// Streams one capture session over the existing peer connection.
///
/// Returns why it ended; reopening is the caller's decision, which is what keeps
/// a recorder restart inside the session instead of turning it into a reconnect.
async fn stream(
    state: &Arc<State>,
    writer: &mut WireWriter,
    control: &mut mpsc::Receiver<Control>,
    cancel: &CancellationToken,
    publisher: &Publisher,
    codec: Codec,
) -> StreamEnd {
    loop {
        let (message, unit) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return StreamEnd::Cancelled,
            value = control.recv() => match value {
                Some(Control::Reply(message)) => (message, None),
                Some(Control::Keyframe) => {
                    if publisher.request_keyframe().is_err() {
                        return StreamEnd::Capture;
                    }
                    continue;
                }
                None => return StreamEnd::PeerGone,
            },
            // A peer that unsubscribed from this display keeps its session but
            // receives no more frames until it subscribes again; the branch is
            // disabled rather than the stream ended.
            unit = publisher.recv(cancel), if subscribed(state) => match unit {
                Ok(Some(unit)) => match video(&unit, codec) {
                    Ok(message) => (message, Some(unit)),
                    Err(_) => return StreamEnd::Capture,
                },
                Ok(None) => return StreamEnd::Capture,
                Err(_) => return StreamEnd::Capture,
            },
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return StreamEnd::Cancelled,
            sent = writer.send(&message) => {
                if sent.is_err() {
                    return StreamEnd::Transport;
                }
            }
        }
        // The publisher's AU credit guard stays alive through the send. Only
        // compressed Bytes are cloned into protobuf; no pixels are touched.
        if let Some(unit) = unit {
            state.update(|s| {
                s.phase = "streaming";
                s.sent_units += 1;
                s.sent_bytes += unit.data.len() as u64;
            });
        }
    }
}

/// Waits before reopening capture, with a bounded delay. False when cancelled.
async fn backoff(cancel: &CancellationToken, restarts: u32) -> bool {
    let delay = Duration::from_millis(500 * u64::from(restarts.min(4) + 1));
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

#[cfg(test)]
mod tests {

    use super::{
        ApproveMode, Disposition, Dropped, HostOptions, InputSink, LocalGrant, Message,
        PrimaryPolicy, accepted_targets, classify, host_passwords, host_policy, local_permissions,
        salted_password,
    };
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
            bitrate_auto: false,
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

    /// The one-time password is salted with the salt this session announces,
    /// which is what a client's first hash is built against.
    #[test]
    fn the_one_time_password_is_salted_with_the_session_salt() {
        let permanent = salted_password(b"permanent", "stored-salt");
        assert_eq!(
            host_passwords("session-salt", "123456", Some(permanent), true, true),
            vec![permanent, salted_password(b"123456", "session-salt")],
            "both configured passwords are accepted at once"
        );
        assert_eq!(
            host_passwords("session-salt", "123456", None, true, true),
            vec![salted_password(b"123456", "session-salt")]
        );
        // The method the operator picked decides which of the two counts.
        assert!(host_passwords("session-salt", "123456", None, true, false).is_empty());
        assert_eq!(
            host_passwords("session-salt", "", Some(permanent), true, false),
            vec![permanent]
        );
        assert!(host_passwords("session-salt", "", None, true, true).is_empty());
    }

    /// The approval policy follows the operator's setting, as the original host.
    #[test]
    fn the_approval_mode_maps_to_the_host_policy() {
        assert_eq!(
            host_policy(ApproveMode::Password),
            PrimaryPolicy::PasswordOnly
        );
        assert_eq!(host_policy(ApproveMode::Click), PrimaryPolicy::ClickOnly);
        assert_eq!(
            host_policy(ApproveMode::Both),
            PrimaryPolicy::PasswordOrClick
        );
    }

    /// The controller's ordinary actions must not end the session.
    ///
    /// The original host skips an injection it is not allowed to perform and
    /// keeps the connection; a close here is seen by the far side as a dropped
    /// session, which turns into a reconnect and a fresh password prompt.
    #[test]
    fn benign_peer_records_never_close_the_session() {
        let mut mouse = Message::new();
        mouse.set_mouse_event(hbb_common::message_proto::MouseEvent {
            x: 10,
            y: 10,
            ..Default::default()
        });
        let mut key = Message::new();
        key.set_key_event(hbb_common::message_proto::KeyEvent {
            down: true,
            ..Default::default()
        });
        let mut resolution = Message::new();
        let mut misc = hbb_common::message_proto::Misc::new();
        misc.set_change_resolution(hbb_common::message_proto::Resolution::default());
        resolution.set_misc(misc);
        let mut other_display = Message::new();
        let mut misc = hbb_common::message_proto::Misc::new();
        misc.set_capture_displays(hbb_common::message_proto::CaptureDisplays {
            add: vec![1],
            ..Default::default()
        });
        other_display.set_misc(misc);
        for message in [&mouse, &key, &resolution, &other_display] {
            let disposition = classify(message, 1920, 1080);
            assert!(
                !matches!(disposition, Disposition::Close),
                "a benign record must keep the session: {:?}",
                message.union
            );
        }
        assert!(matches!(classify(&mouse, 1920, 1080), Disposition::Input));
        assert!(matches!(
            classify(&resolution, 1920, 1080),
            Disposition::Dropped(Dropped::Display)
        ));
    }

    /// A display subscription change starts or stops the stream, not the session.
    #[test]
    fn a_subscription_change_only_moves_the_stream() {
        let mut unsubscribe = Message::new();
        let mut misc = hbb_common::message_proto::Misc::new();
        misc.set_capture_displays(hbb_common::message_proto::CaptureDisplays {
            sub: vec![0],
            ..Default::default()
        });
        unsubscribe.set_misc(misc);
        assert!(matches!(
            classify(&unsubscribe, 1920, 1080),
            Disposition::Subscribed(false)
        ));

        let mut subscribe = Message::new();
        let mut misc = hbb_common::message_proto::Misc::new();
        misc.set_capture_displays(hbb_common::message_proto::CaptureDisplays {
            add: vec![0],
            ..Default::default()
        });
        subscribe.set_misc(misc);
        assert!(matches!(
            classify(&subscribe, 1920, 1080),
            Disposition::Subscribed(true)
        ));
    }

    /// A close request, a removed feature and a keepalive keep their exact meaning.
    #[test]
    fn a_close_request_is_the_only_record_that_ends_the_session() {
        let mut close = Message::new();
        let mut misc = hbb_common::message_proto::Misc::new();
        misc.set_close_reason("".into());
        close.set_misc(misc);
        assert!(matches!(classify(&close, 1920, 1080), Disposition::Close));

        let mut chat = Message::new();
        let mut misc = hbb_common::message_proto::Misc::new();
        misc.set_chat_message(hbb_common::message_proto::ChatMessage::default());
        chat.set_misc(misc);
        assert!(matches!(
            classify(&chat, 1920, 1080),
            Disposition::Dropped(Dropped::Chat)
        ));

        let mut probe = Message::new();
        probe.set_test_delay(hbb_common::message_proto::TestDelay {
            from_client: true,
            ..Default::default()
        });
        assert!(matches!(
            classify(&probe, 1920, 1080),
            Disposition::Reply(_)
        ));
    }

    /// A peer sends the address it was handed, not this socket's bind address.
    #[test]
    fn a_wildcard_listener_still_accepts_the_advertised_ip() {
        let mut options = options(true, Some(counting_sink()));
        options.listen = "0.0.0.0:21118".parse().unwrap();
        options.id = "192.168.108.155:21118".into();
        let local: std::net::SocketAddr = "10.53.176.3:21118".parse().unwrap();
        let targets = accepted_targets(&options, local);
        // The bare IP is the form a direct client sends; the id keeps its port.
        assert!(
            targets.iter().any(|t| t == "192.168.108.155"),
            "bare advertised IP must be accepted, got {targets:?}"
        );
        assert!(
            targets.iter().any(|t| t == "192.168.108.155:21118"),
            "advertised id must be accepted, got {targets:?}"
        );
        assert!(
            !targets
                .iter()
                .any(|t| t == "0.0.0.0" || t == "0.0.0.0:21118"),
            "a bind address is not an identity, got {targets:?}"
        );
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
