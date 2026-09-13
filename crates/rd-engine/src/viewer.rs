//! First original-peer direct TCP viewer. No legacy Connection/VideoHandler.
//! Surface ownership comes from the trusted HAR, not a retained numeric ID.
//! RX, one writer, and one ordered video feeder are independent persistent tasks.
//! SDK creation/destruction run on blocking workers; compressed admission waits
//! are cancellation-aware notifications, never sleep retries or frame tasks.

#[path = "platform/ohos_decoder.rs"]
pub mod decoder;
use crate::{
    handshake::ViewerIdentity,
    media_capability::{self, AdvertisedCodec, CapabilityError},
    rendezvous::{RendezvousConfig, RouteKind},
    session::{AuthenticatedParts, ViewerEvent, ViewerSession},
    transport::{WireReader, WireWriter},
};
pub use decoder::SurfaceLease;
use decoder::{
    AccessUnit, AccessUnitKind, DecoderConfig, DecoderError, DecoderObserver, SurfaceDecoder,
    VideoCodec,
};
use hbb_common::message_proto::{
    self as proto, Clipboard, ClipboardFormat, ControlKey, EncodedVideoFrame, KeyEvent,
    KeyboardMode, LoginRequest, Message, Misc, MouseEvent, MultiClipboards, OptionMessage,
    PeerInfo, SupportedDecoding, SwitchDisplay, message, misc, option_message::BoolOption,
    permission_info::Permission, supported_decoding::PreferCodec, video_frame,
};
use std::{
    collections::VecDeque,
    fmt,
    io::Read,
    net::SocketAddr,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{
    runtime::Runtime,
    sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

// End-to-end low-latency budget: one record may be feeding while one waits.
// Larger queues preserve throughput by displaying increasingly stale frames.
const VIDEO_RECORDS: usize = 2;
const VIDEO_BYTES: usize = 32 * 1024 * 1024;
const MAX_UNITS_PER_RECORD: usize = 256;
const COMMANDS: usize = 128;
const AUTH_TIMEOUT: Duration = Duration::from_secs(120);
// Local feature policy for this explicitly started interactive viewer. It is
// NEVER granted by a peer report. Keyboard/text/clipboard are implemented, so
// they are requested here and still require the peer to enable them.
const LOCAL_MOUSE_ENABLED: bool = true;
const LOCAL_KEYBOARD_ENABLED: bool = LOCAL_MOUSE_ENABLED;
const LOCAL_CLIPBOARD_ENABLED: bool = true;
/// Bound for a single outbound text clipboard payload, before compression.
const MAX_CLIPBOARD_TEXT: usize = 1024 * 1024;
/// Bound for one decompressed inbound text clipboard payload.
const MAX_INBOUND_CLIPBOARD_TEXT: usize = 4 * 1024 * 1024;
/// How many inbound clipboard payloads stay readable through `take_clipboard`.
const CLIPBOARD_QUEUE: usize = 4;

/// Peer-reported image quality. Only a value explicitly sent by the peer is
/// stored; the local default is never reported as a peer decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerImageQuality {
    Low,
    Balanced,
    Best,
    Custom(i32),
}

impl ViewerImageQuality {
    fn proto_quality(self) -> proto::ImageQuality {
        match self {
            Self::Low => proto::ImageQuality::Low,
            Self::Balanced => proto::ImageQuality::Balanced,
            Self::Best => proto::ImageQuality::Best,
            Self::Custom(_) => proto::ImageQuality::Balanced,
        }
    }

    fn custom_quality(self) -> Option<i32> {
        match self {
            Self::Custom(value) if (10..=2000).contains(&value) => Some(value),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Balanced => "balanced",
            Self::Best => "best",
            Self::Custom(_) => "custom",
        }
    }
}

impl Default for ViewerImageQuality {
    fn default() -> Self {
        Self::Balanced
    }
}

/// One legacy-mode key identity. `Character` carries the raw key code the
/// original protocol expects for `chr`; `Control` carries a protocol control key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerKey {
    Character(u32),
    Control(ControlKey),
}

fn key_message(event: KeyEvent) -> Message {
    let mut message = Message::new();
    message.set_key_event(event);
    message
}

/// Original-protocol legacy modifiers, in the peer's documented order.
fn legacy_modifiers(event: &mut KeyEvent, alt: bool, ctrl: bool, shift: bool, command: bool) {
    if alt {
        event.modifiers.push(ControlKey::Alt.into());
    }
    if shift {
        event.modifiers.push(ControlKey::Shift.into());
    }
    if ctrl {
        event.modifiers.push(ControlKey::Control.into());
    }
    if command {
        event.modifiers.push(ControlKey::Meta.into());
    }
}

/// Map an original-protocol legacy key name onto a key identity. A single
/// character is sent verbatim as its own `chr` code, matching the original
/// client's `KEY_MAP` fallback; anything else must name a protocol control key.
pub fn legacy_key_name(name: &str) -> Option<ViewerKey> {
    let mut chars = name.chars();
    if let (Some(character), None) = (chars.next(), chars.next()) {
        return u32::try_from(character as u32)
            .ok()
            .map(ViewerKey::Character);
    }
    control_key_name(name).map(ViewerKey::Control)
}

fn control_key_name(name: &str) -> Option<ControlKey> {
    use ControlKey::*;
    Some(match name {
        "Alt" | "RAlt" | "VK_MENU" => Alt,
        "Backspace" | "VK_BACK" => Backspace,
        "CapsLock" | "VK_CAPITAL" => CapsLock,
        "Control" | "VK_CONTROL" => Control,
        "RControl" => RControl,
        "Shift" | "VK_SHIFT" => Shift,
        "RShift" => RShift,
        "Meta" | "RWin" => Meta,
        "Delete" | "VK_DELETE" => Delete,
        "DownArrow" | "VK_DOWN" => DownArrow,
        "End" | "VK_END" => End,
        "Escape" | "VK_ESCAPE" => Escape,
        "F1" | "VK_F1" => F1,
        "F2" | "VK_F2" => F2,
        "F3" | "VK_F3" => F3,
        "F4" | "VK_F4" => F4,
        "F5" | "VK_F5" => F5,
        "F6" | "VK_F6" => F6,
        "F7" | "VK_F7" => F7,
        "F8" | "VK_F8" => F8,
        "F9" | "VK_F9" => F9,
        "F10" | "VK_F10" => F10,
        "F11" | "VK_F11" => F11,
        "F12" | "VK_F12" => F12,
        "Home" | "VK_HOME" => Home,
        "LeftArrow" | "VK_LEFT" => LeftArrow,
        "PageDown" | "VK_NEXT" => PageDown,
        "PageUp" | "VK_PRIOR" => PageUp,
        "Return" | "VK_RETURN" | "VK_ENTER" => Return,
        "NumpadEnter" => NumpadEnter,
        "RightArrow" | "VK_RIGHT" => RightArrow,
        "Space" | "VK_SPACE" => Space,
        "Tab" | "VK_TAB" => Tab,
        "UpArrow" | "VK_UP" => UpArrow,
        "Numpad0" | "VK_NUMPAD0" => Numpad0,
        "Numpad1" | "VK_NUMPAD1" => Numpad1,
        "Numpad2" | "VK_NUMPAD2" => Numpad2,
        "Numpad3" | "VK_NUMPAD3" => Numpad3,
        "Numpad4" | "VK_NUMPAD4" => Numpad4,
        "Numpad5" | "VK_NUMPAD5" => Numpad5,
        "Numpad6" | "VK_NUMPAD6" => Numpad6,
        "Numpad7" | "VK_NUMPAD7" => Numpad7,
        "Numpad8" | "VK_NUMPAD8" => Numpad8,
        "Numpad9" | "VK_NUMPAD9" => Numpad9,
        "Cancel" | "VK_CANCEL" => Cancel,
        "Clear" | "VK_CLEAR" => Clear,
        "Pause" | "VK_PAUSE" => Pause,
        "Insert" | "VK_INSERT" => Insert,
        "Print" | "VK_PRINT" => Print,
        "Snapshot" | "VK_SNAPSHOT" => Snapshot,
        "Scroll" | "VK_SCROLL" => Scroll,
        "NumLock" => NumLock,
        "Apps" => Apps,
        "Multiply" | "VK_MULTIPLY" => Multiply,
        "Add" | "VK_ADD" => Add,
        "Subtract" | "VK_SUBTRACT" => Subtract,
        "Decimal" | "VK_DECIMAL" => Decimal,
        "Divide" | "VK_DIVIDE" => Divide,
        "CTRL_ALT_DEL" => CtrlAltDel,
        "LOCK_SCREEN" => LockScreen,
        _ => return None,
    })
}

/// USB HID usage code to a key identity. Printable keys keep their ASCII `chr`
/// value so the peer's own layout resolution stays authoritative; only
/// non-printable keys become protocol control keys.
pub fn usb_hid_key(usb_hid: u32, character: &str) -> Option<ViewerKey> {
    use ControlKey::*;
    let control = match usb_hid {
        0x28 => Some(Return),
        0x29 => Some(Escape),
        0x2a => Some(Backspace),
        0x2b => Some(Tab),
        0x39 => Some(CapsLock),
        0x3a..=0x45 => {
            Some([F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12][(usb_hid - 0x3a) as usize])
        }
        0x48 => Some(Pause),
        0x49 => Some(Insert),
        0x4a => Some(Home),
        0x4b => Some(PageUp),
        0x4c => Some(Delete),
        0x4d => Some(End),
        0x4e => Some(PageDown),
        0x4f => Some(RightArrow),
        0x50 => Some(LeftArrow),
        0x51 => Some(DownArrow),
        0x52 => Some(UpArrow),
        0x53 => Some(NumLock),
        0x54 => Some(Divide),
        0x55 => Some(Multiply),
        0x56 => Some(Subtract),
        0x57 => Some(Add),
        0x58 => Some(NumpadEnter),
        0x59..=0x61 => Some(
            [
                Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
            ][(usb_hid - 0x59) as usize],
        ),
        0x62 => Some(Numpad0),
        0x63 => Some(Decimal),
        0x65 => Some(Apps),
        0x66 => Some(Power),
        0x67 => Some(Equals),
        0xe0 => Some(Control),
        0xe1 => Some(Shift),
        0xe2 => Some(Alt),
        0xe3 => Some(Meta),
        0xe4 => Some(RControl),
        0xe5 => Some(RShift),
        0xe6 => Some(RAlt),
        0xe7 => Some(RWin),
        _ => None,
    };
    if let Some(control) = control {
        return Some(ViewerKey::Control(control));
    }
    // A printable event character is authoritative; for a layout-unknown
    // keycode with no character, fall back to the US printable usage map.
    if character.len() == 1 {
        if let Some(character) = character.chars().next() {
            if !character.is_control() {
                return u32::try_from(character as u32)
                    .ok()
                    .map(ViewerKey::Character);
            }
        }
    }
    const PRINTABLE: &[u8] = b"abcdefghijklmnopqrstuvwxyz1234567890";
    if let Some(index) = usize::try_from(usb_hid)
        .ok()
        .and_then(|code| code.checked_sub(0x04))
        .filter(|index| *index < PRINTABLE.len())
    {
        return Some(ViewerKey::Character(PRINTABLE[index] as u32));
    }
    const PUNCTUATION: [(u32, char); 11] = [
        (0x2d, '-'),
        (0x2e, '='),
        (0x2f, '['),
        (0x30, ']'),
        (0x31, '\\'),
        (0x33, ';'),
        (0x34, '\''),
        (0x35, '`'),
        (0x36, ','),
        (0x37, '.'),
        (0x38, '/'),
    ];
    PUNCTUATION
        .iter()
        .find(|(code, _)| *code == usb_hid)
        .map(|(_, character)| ViewerKey::Character(*character as u32))
}

// Deliberately no Debug: credentials and peer identities must not enter logs.
pub struct ViewerOptions {
    /// Present only for direct TCP. ID/rendezvous resolves its own peer route.
    pub address: Option<SocketAddr>,
    pub username: String,
    pub local_id: String,
    pub local_name: String,
    pub password: String,
    pub requested_fps: u32,
    /// Outbound remote copy/paste text. Remote paste is the peer's own action.
    pub clipboard_enabled: bool,
    pub image_quality: ViewerImageQuality,
}
impl Drop for ViewerOptions {
    fn drop(&mut self) {
        let mut password = std::mem::take(&mut self.password).into_bytes();
        hbb_common::sodiumoxide::utils::memzero(&mut password);
    }
}

#[derive(Debug, Clone, Default)]
pub struct ViewerSnapshot {
    pub phase: String,
    pub error: Option<String>,
    pub width: i32,
    pub height: i32,
    pub codec: String,
    /// `direct_tcp`, `tcp_hole_punch` or `relay`; never inferred from an address.
    pub route: String,
    pub received_units: u64,
    pub pushed_units: u64,
    pub render_submissions: u64,
    /// Units that carried a timestamp and contributed to the totals below.
    pub timed_units: u64,
    /// Total decoder-to-render latency in microseconds across those units.
    /// Divided by `timed_units` this is the average the decoder is responsible
    /// for, which is the part of the path this process controls.
    pub pipeline_micros_total: u64,
    /// The worst single latency in microseconds. Averages hide the stalls a
    /// viewer actually notices, so both are reported.
    pub pipeline_micros_peak: u64,
    /// Arrival-to-render-submission latency in microseconds for the frames
    /// counted below. This is the path an operator waits through and it is
    /// several times the decoder's own work; the `pipeline_micros_*` figures
    /// above measure the submission call alone and are not this.
    pub render_micros_total: u64,
    /// The worst single arrival-to-render latency in microseconds.
    pub render_micros_peak: u64,
    /// Frames that carried a matching arrival stamp and are counted above.
    pub render_timed_units: u64,
    /// Time units spent waiting for a place in the admission queue, in
    /// microseconds. This is not decode work: it is the backlog that grows when
    /// the peer sends faster than this device decodes, and reporting it as
    /// decode time is what made the figure read absurdly high.
    pub queue_micros_total: u64,
    pub queue_timed_units: u64,
    /// Units waiting for the decoder right now.
    pub queued_units: u64,
    pub keyboard_allowed: bool,
    /// Peer-side clipboard permission. Local policy is separate.
    pub clipboard_allowed: bool,
    pub image_quality: String,
    pub requested_fps: u32,
    pub closed: bool,
    /// Actual authenticated connection evidence, never inferred from options.
    pub encrypted: bool,
    pub peer_verified: bool,
}

#[derive(Debug, Clone)]
pub enum ViewerError {
    InvalidOptions,
    UnsupportedPlatform,
    NoHardwareCodec,
    RuntimeUnavailable,
    ConnectFailed,
    RendezvousFailed,
    AuthenticationFailed,
    SecondFactorUnsupported,
    TransportFailed,
    RemoteClosed,
    InvalidPeerGeometry,
    UnsupportedCodec,
    InvalidVideoRecord,
    VideoRecordTooLarge,
    TimestampOverflow,
    InputNotAllowed,
    InvalidMouse,
    NotAuthenticated,
    Backpressure,
    Closed,
    TaskFailed,
    ReclamationUnconfirmed,
    Decoder(DecoderError),
    /// A VNC session failed or refused an operation; the text says which.
    Vnc(crate::vnc::VncError),
    /// An RDP session failed or is not implemented yet.
    Rdp(crate::rdp::RdpError),
}
impl fmt::Display for ViewerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // All variants contain local constants/native error codes only. Never
        // embed peer LoginError/MessageBox/CloseReason strings or I/O diagnostics.
        match self {
            Self::Decoder(error) => write!(f, "Surface decoder: {error:?}"),
            Self::Vnc(error) => write!(f, "{error}"),
            Self::Rdp(error) => write!(f, "{error}"),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for ViewerError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    display: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}
struct Inner {
    snapshot: ViewerSnapshot,
    geometry: Option<Geometry>,
    decoder: Option<DecoderObserver>,
    /// Inbound text clipboards awaiting a local read, oldest first.
    clipboard: VecDeque<String>,
}
struct State {
    inner: Mutex<Inner>,
    cancel: CancellationToken,
    done: Notify,
    result: Mutex<Option<Result<(), ViewerError>>>,
    authenticated: AtomicBool,
    remote_keyboard: AtomicBool,
    /// Peer-side clipboard permission, independent from local policy.
    remote_clipboard: AtomicBool,
    /// Local policy: only an explicitly started viewer may write the peer's
    /// clipboard, and only after the peer granted the permission.
    local_clipboard: bool,
    /// Live requested FPS; the login request carries the initial value and a
    /// later user change is pushed as an OptionMessage.
    requested_fps: AtomicU32,
    /// Live requested image quality; same ownership split as `requested_fps`.
    image_quality: Mutex<ViewerImageQuality>,
    /// Authoritative peer metadata from the accepted login response. Only the
    /// peer's own report is stored; nothing here is inferred from options.
    peer_info: Mutex<Option<PeerInfo>>,
    /// Live outbound controls. `None` until authentication completes; every
    /// sender revalidates instead of trusting the enqueue-time snapshot.
    control: Mutex<Option<mpsc::Sender<Message>>>,
    // Independent from operational/auth/network errors. Set BEFORE creating a
    // native decoder and cleared only by confirmed destruction/no allocation.
    resources_unconfirmed: AtomicBool,
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    fn phase(&self, phase: &str) {
        lock(&self.inner).snapshot.phase = phase.into();
    }
    fn geometry(&self, geometry: Geometry) {
        let mut inner = lock(&self.inner);
        inner.geometry = Some(geometry);
        inner.snapshot.width = geometry.width;
        inner.snapshot.height = geometry.height;
    }
    fn route(&self, route: &'static str) {
        lock(&self.inner).snapshot.route = route.into();
    }
    fn snapshot(&self) -> ViewerSnapshot {
        let inner = lock(&self.inner);
        let mut out = inner.snapshot.clone();
        out.requested_fps = self.requested_fps.load(Ordering::Acquire);
        out.image_quality = self
            .image_quality
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .label()
            .to_owned();
        out.keyboard_allowed = LOCAL_MOUSE_ENABLED
            && self.authenticated.load(Ordering::Acquire)
            && self.remote_keyboard.load(Ordering::Acquire)
            && !self.cancel.is_cancelled();
        out.clipboard_allowed = LOCAL_CLIPBOARD_ENABLED
            && self.local_clipboard
            && self.authenticated.load(Ordering::Acquire)
            && self.remote_clipboard.load(Ordering::Acquire)
            && !self.cancel.is_cancelled();
        if let Some(observer) = &inner.decoder {
            if let Ok(stats) = observer.stats() {
                out.pushed_units = out.pushed_units.saturating_add(stats.pushed_units);
                out.render_submissions = out
                    .render_submissions
                    .saturating_add(stats.render_submissions);
                out.timed_units = out.timed_units.saturating_add(stats.timed_units);
                out.pipeline_micros_total = out
                    .pipeline_micros_total
                    .saturating_add(stats.pipeline_micros_total);
                out.pipeline_micros_peak = out.pipeline_micros_peak.max(stats.pipeline_micros_peak);
                out.render_micros_total = out
                    .render_micros_total
                    .saturating_add(stats.render_micros_total);
                out.render_micros_peak = out.render_micros_peak.max(stats.render_micros_peak);
                out.render_timed_units = out
                    .render_timed_units
                    .saturating_add(stats.render_timed_units);
                out.queue_micros_total = out
                    .queue_micros_total
                    .saturating_add(stats.queue_micros_total);
                out.queue_timed_units = out
                    .queue_timed_units
                    .saturating_add(stats.queue_timed_units);
                // A gauge, so the deepest of the observes rather than a sum.
                out.queued_units = out.queued_units.max(observer.queued_units() as u64);
                if let Some(error) = stats.failure {
                    out.error = Some(ViewerError::Decoder(error).to_string());
                }
                if stats.render_submissions > 0
                    && out.error.is_none()
                    && !self.cancel.is_cancelled()
                    && !out.closed
                {
                    out.phase = "streaming".into();
                }
            }
        }
        out
    }
    /// Live control sender. `None` before authentication so no message can be
    /// written on a connection that has not completed the login handshake.
    fn control(&self) -> Option<mpsc::Sender<Message>> {
        lock(&self.control).clone()
    }
    /// Read-only copy of the peer's own report, when authentication succeeded.
    pub fn peer_info(&self) -> Option<PeerInfo> {
        lock(&self.peer_info).clone()
    }
    fn take_clipboard(&self) -> Option<String> {
        lock(&self.inner).clipboard.pop_front()
    }
    fn geometry_is_none(&self) -> bool {
        lock(&self.inner).geometry.is_none()
    }
    fn request_close(&self) {
        self.authenticated.store(false, Ordering::Release);
        self.cancel.cancel();
        let mut inner = lock(&self.inner);
        if !inner.snapshot.closed {
            inner.snapshot.phase = "closing".into();
        }
        if let Some(observer) = &inner.decoder {
            observer.request_close();
        }
    }
    fn finish(&self, result: Result<(), ViewerError>) {
        self.authenticated.store(false, Ordering::Release);
        let reclamation = if self.resources_unconfirmed.load(Ordering::Acquire) {
            Err(match &result {
                Err(error @ ViewerError::Decoder(DecoderError::DestroyFailed { .. })) => {
                    error.clone()
                }
                _ => ViewerError::ReclamationUnconfirmed,
            })
        } else {
            Ok(())
        };
        let mut inner = lock(&self.inner);
        inner.snapshot.closed = true;
        inner.snapshot.keyboard_allowed = false;
        inner.snapshot.error = result
            .as_ref()
            .err()
            .or_else(|| reclamation.as_ref().err())
            .map(ToString::to_string);
        inner.snapshot.phase = if inner.snapshot.error.is_none() {
            "closed"
        } else {
            "failed"
        }
        .into();
        // HAR uses ONLY this result to decide whether it may release its lease
        // and registry slot. Old wrong-password/disconnect errors are not a UAF.
        *lock(&self.result) = Some(reclamation);
        self.done.notify_waiters();
    }
}

pub struct Viewer {
    state: Arc<State>,
    mouse: mpsc::Sender<Message>,
    authentication: mpsc::Sender<AuthCommand>,
}

enum Connector {
    Direct(ViewerIdentity),
    Rendezvous(RendezvousConfig),
}

enum AuthCommand {
    Password(Password),
    SecondFactor(Password),
    ContinueInsecure(bool),
}

fn runtime() -> Result<&'static Runtime, ViewerError> {
    crate::executor::runtime().map_err(|_| ViewerError::RuntimeUnavailable)
}

impl Viewer {
    pub fn start(
        options: ViewerOptions,
        lease: Arc<dyn SurfaceLease>,
    ) -> Result<Arc<Self>, ViewerError> {
        if options.address.is_none_or(|address| address.port() == 0) {
            return Err(ViewerError::InvalidOptions);
        }
        Self::start_with_connector(
            options,
            lease,
            Connector::Direct(ViewerIdentity::LegacyUnverified),
        )
    }

    /// Trust must come from an explicit out-of-band peer pin, not this socket.
    pub fn start_verified(
        options: ViewerOptions,
        lease: Arc<dyn SurfaceLease>,
        expected_id: String,
        peer_signing_key: hbb_common::sodiumoxide::crypto::sign::PublicKey,
    ) -> Result<Arc<Self>, ViewerError> {
        if expected_id.is_empty() || expected_id.len() > 512 {
            return Err(ViewerError::InvalidOptions);
        }
        if options.address.is_none_or(|address| address.port() == 0) {
            return Err(ViewerError::InvalidOptions);
        }
        Self::start_with_connector(
            options,
            lease,
            Connector::Direct(ViewerIdentity::PinnedHost {
                expected_id,
                peer_signing_key,
            }),
        )
    }

    /// Original hbbs/hbbr ID route with mandatory server-signed peer identity.
    /// The dialer completes SignedId/box/secretbox before password authentication.
    pub fn start_rendezvous(
        options: ViewerOptions,
        lease: Arc<dyn SurfaceLease>,
        config: RendezvousConfig,
    ) -> Result<Arc<Self>, ViewerError> {
        if options.address.is_some() || options.username != config.id {
            return Err(ViewerError::InvalidOptions);
        }
        Self::start_with_connector(options, lease, Connector::Rendezvous(config))
    }

    fn start_with_connector(
        options: ViewerOptions,
        lease: Arc<dyn SurfaceLease>,
        connector: Connector,
    ) -> Result<Arc<Self>, ViewerError> {
        if options.username.is_empty()
            || options.local_id.is_empty()
            || options.username.len() > 512
            || options.local_id.len() > 512
            || options.local_name.len() > 512
            || options.password.len() > 4096
            || !(1..=240).contains(&options.requested_fps)
        {
            return Err(ViewerError::InvalidOptions);
        }
        let runtime = runtime()?;
        let (mouse, mouse_rx) = mpsc::channel(COMMANDS);
        let (authentication, authentication_rx) = mpsc::channel(4);
        let requested_fps = options.requested_fps;
        let image_quality = options.image_quality;
        let local_clipboard = options.clipboard_enabled;
        let state = Arc::new(State {
            inner: Mutex::new(Inner {
                snapshot: ViewerSnapshot {
                    phase: "capability".into(),
                    ..Default::default()
                },
                geometry: None,
                decoder: None,
                clipboard: VecDeque::new(),
            }),
            cancel: CancellationToken::new(),
            done: Notify::new(),
            result: Mutex::new(None),
            authenticated: AtomicBool::new(false),
            remote_keyboard: AtomicBool::new(false),
            remote_clipboard: AtomicBool::new(false),
            local_clipboard,
            requested_fps: AtomicU32::new(requested_fps),
            image_quality: Mutex::new(image_quality),
            peer_info: Mutex::new(None),
            control: Mutex::new(None),
            resources_unconfirmed: AtomicBool::new(false),
        });
        let viewer = Arc::new(Self {
            state: state.clone(),
            mouse,
            authentication,
        });
        // Tasks hold State, NOT Viewer: dropping the last UI/HAR Viewer triggers
        // cancellation instead of a self-retaining session/Surface reference.
        runtime.spawn(async move {
            // Keep one final lease until run/decoder teardown ends, including
            // authentication failures; release that reference off the IO thread.
            let mut result = run(
                state.clone(),
                options,
                lease.clone(),
                mouse_rx,
                authentication_rx,
                connector,
            )
            .await;
            if tokio::task::spawn_blocking(move || drop(lease))
                .await
                .is_err()
            {
                state.resources_unconfirmed.store(true, Ordering::Release);
                if result.is_ok() {
                    result = Err(ViewerError::TaskFailed);
                }
            }
            state.finish(result);
        });
        Ok(viewer)
    }
    pub fn snapshot(&self) -> ViewerSnapshot {
        self.state.snapshot()
    }

    /// The peer's own accepted report: platform, version, name and displays.
    /// `None` until authentication completes.
    pub fn peer_info(&self) -> Option<PeerInfo> {
        self.state.peer_info()
    }

    pub fn submit_password(&self, password: String) -> Result<(), ViewerError> {
        if password.is_empty() || password.len() > 4096 {
            return Err(ViewerError::InvalidOptions);
        }
        self.authentication
            .try_send(AuthCommand::Password(Password(password.into_bytes())))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ViewerError::Backpressure,
                mpsc::error::TrySendError::Closed(_) => ViewerError::Closed,
            })
    }

    pub fn submit_second_factor(&self, code: String) -> Result<(), ViewerError> {
        if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ViewerError::InvalidOptions);
        }
        self.authentication
            .try_send(AuthCommand::SecondFactor(Password(code.into_bytes())))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ViewerError::Backpressure,
                mpsc::error::TrySendError::Closed(_) => ViewerError::Closed,
            })
    }

    pub fn continue_insecure(&self, allow: bool) -> Result<(), ViewerError> {
        self.authentication
            .try_send(AuthCommand::ContinueInsecure(allow))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ViewerError::Backpressure,
                mpsc::error::TrySendError::Closed(_) => ViewerError::Closed,
            })
    }

    /// Whether outbound keyboard/text input is currently authorized locally and
    /// by the peer. Callers must still handle a rejection at send time.
    pub fn input_allowed(&self) -> bool {
        LOCAL_KEYBOARD_ENABLED
            && self.state.authenticated.load(Ordering::Acquire)
            && self.state.remote_keyboard.load(Ordering::Acquire)
            && !self.state.cancel.is_cancelled()
    }

    /// Whether pointer input is currently authorized, and if not, which gate is
    /// closed. The boolean mouse entry point cannot explain a refusal, and "the
    /// cursor does not move" is otherwise indistinguishable from broken mapping.
    pub fn mouse_refusal(&self) -> Option<&'static str> {
        if !LOCAL_MOUSE_ENABLED {
            return Some("local_mouse_disabled");
        }
        if self.state.cancel.is_cancelled() {
            return Some("cancelled");
        }
        if !self.state.authenticated.load(Ordering::Acquire) {
            return Some("not_authenticated");
        }
        if !self.state.remote_keyboard.load(Ordering::Acquire) {
            return Some("peer_permission_denied");
        }
        if self.state.geometry_is_none() {
            return Some("geometry_missing");
        }
        None
    }

    /// Send one legacy-mode key transition. `modifiers` are the legacy modifier
    /// bits (alt, ctrl, shift, meta) as the original protocol defines them.
    pub fn send_key(
        &self,
        key: ViewerKey,
        down: bool,
        press: bool,
        alt: bool,
        ctrl: bool,
        shift: bool,
        command: bool,
    ) -> Result<(), ViewerError> {
        if !self.input_allowed() {
            return Err(ViewerError::InputNotAllowed);
        }
        let mut event = KeyEvent {
            down,
            press,
            mode: KeyboardMode::Legacy.into(),
            ..Default::default()
        };
        match key {
            ViewerKey::Character(value) => event.set_chr(value),
            ViewerKey::Control(value) => event.set_control_key(value),
        }
        legacy_modifiers(&mut event, alt, ctrl, shift, command);
        self.enqueue(key_message(event))
    }

    /// Send a whole string through the protocol's sequence key event. This is
    /// how non-ASCII/IME text reaches the peer without local key synthesis.
    pub fn send_text(&self, value: String) -> Result<(), ViewerError> {
        if !self.input_allowed() {
            return Err(ViewerError::InputNotAllowed);
        }
        if value.is_empty() || value.len() > MAX_CLIPBOARD_TEXT {
            return Err(ViewerError::InvalidOptions);
        }
        let mut event = KeyEvent::new();
        event.set_seq(value);
        self.enqueue(key_message(event))
    }

    /// Outbound text clipboard. Requires local policy AND the peer permission;
    /// compression only happens when it actually shrinks the payload.
    pub fn send_clipboard_text(&self, text: String) -> Result<(), ViewerError> {
        if !LOCAL_CLIPBOARD_ENABLED
            || !self.state.local_clipboard
            || !self.state.authenticated.load(Ordering::Acquire)
            || !self.state.remote_clipboard.load(Ordering::Acquire)
            || self.state.cancel.is_cancelled()
        {
            return Err(ViewerError::InputNotAllowed);
        }
        if text.is_empty() || text.len() > MAX_CLIPBOARD_TEXT {
            return Err(ViewerError::InvalidOptions);
        }
        let raw = text.into_bytes();
        let compressed = hbb_common::compress::compress(&raw);
        let (compress, content) = if compressed.is_empty() || compressed.len() >= raw.len() {
            (false, raw)
        } else {
            (true, compressed)
        };
        let mut message = Message::new();
        message.set_multi_clipboards(MultiClipboards {
            clipboards: vec![Clipboard {
                compress,
                content: content.into(),
                format: ClipboardFormat::Text.into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        self.enqueue(message)
    }

    /// Read and clear the oldest inbound text clipboard payload. Returns `None`
    /// when nothing new arrived, so a repeated poll cannot re-apply stale text.
    pub fn take_clipboard_text(&self) -> Option<String> {
        if !LOCAL_CLIPBOARD_ENABLED || !self.state.local_clipboard {
            return None;
        }
        self.state.take_clipboard()
    }

    /// Change the requested capture rate. Before authentication this only edits
    /// the pending login request; afterwards it is pushed to the peer.
    pub fn set_requested_fps(&self, fps: u32) -> Result<(), ViewerError> {
        if !(1..=240).contains(&fps) {
            return Err(ViewerError::InvalidOptions);
        }
        if self.state.authenticated.load(Ordering::Acquire) {
            self.send_option(|option| option.custom_fps = fps as i32)?;
        }
        self.state.requested_fps.store(fps, Ordering::Release);
        Ok(())
    }

    /// Change the requested image quality, live when authenticated.
    pub fn set_image_quality(&self, quality: ViewerImageQuality) -> Result<(), ViewerError> {
        if let ViewerImageQuality::Custom(value) = quality {
            if !(10..=2000).contains(&value) {
                return Err(ViewerError::InvalidOptions);
            }
        }
        if self.state.authenticated.load(Ordering::Acquire) {
            self.send_option(|option| {
                option.image_quality = quality.proto_quality().into();
                if let Some(custom) = quality.custom_quality() {
                    option.custom_image_quality = custom;
                } else {
                    // Clearing the custom value keeps a preset authoritative.
                    option.custom_image_quality = 0;
                }
            })?;
        }
        *self
            .state
            .image_quality
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = quality;
        Ok(())
    }

    /// Ask the peer to switch its captured display and request a refresh of the
    /// new source. The peer answers with the authoritative SwitchDisplay.
    pub fn switch_display(&self, display: i32, width: i32, height: i32) -> Result<(), ViewerError> {
        if display < 0 || width < 0 || height < 0 {
            return Err(ViewerError::InvalidOptions);
        }
        if !self.state.authenticated.load(Ordering::Acquire) || self.state.cancel.is_cancelled() {
            return Err(ViewerError::NotAuthenticated);
        }
        let mut misc = Misc::new();
        misc.set_switch_display(SwitchDisplay {
            display,
            width,
            height,
            ..Default::default()
        });
        let mut message = Message::new();
        message.set_misc(misc);
        self.enqueue(message)?;
        let mut query = Misc::new();
        query.set_message_query(proto::MessageQuery {
            switch_display: display,
            ..Default::default()
        });
        let mut message = Message::new();
        message.set_misc(query);
        self.enqueue(message)
    }

    /// Ask the peer to change the resolution of one display. Zero means the peer
    /// picks its own value from the resolution list it advertised.
    pub fn change_resolution(
        &self,
        display: i32,
        width: i32,
        height: i32,
    ) -> Result<(), ViewerError> {
        if display < 0 || width < 0 || height < 0 {
            return Err(ViewerError::InvalidOptions);
        }
        self.send_option_control(|misc| {
            misc.set_change_display_resolution(proto::DisplayResolution {
                display,
                resolution: Some(proto::Resolution {
                    width,
                    height,
                    ..Default::default()
                })
                .into(),
                ..Default::default()
            });
        })
    }

    /// Ask the peer to resend its current frame.
    pub fn refresh_video(&self, display: i32) -> Result<(), ViewerError> {
        self.send_option_control(|misc| {
            if display < 0 {
                misc.set_refresh_video(true);
            } else {
                misc.set_refresh_video_display(display);
            }
        })
    }

    fn send_option(&self, edit: impl FnOnce(&mut OptionMessage)) -> Result<(), ViewerError> {
        self.send_option_control(|misc| {
            let mut option = OptionMessage::new();
            edit(&mut option);
            misc.set_option(option);
        })
    }

    fn send_option_control(&self, edit: impl FnOnce(&mut Misc)) -> Result<(), ViewerError> {
        if !self.state.authenticated.load(Ordering::Acquire) || self.state.cancel.is_cancelled() {
            return Err(ViewerError::NotAuthenticated);
        }
        let mut misc = Misc::new();
        edit(&mut misc);
        let mut message = Message::new();
        message.set_misc(misc);
        self.enqueue(message)
    }

    /// Queue one control message. Input revocation is rechecked at write time,
    /// so a queued message can never outlive a permission change.
    fn enqueue(&self, message: Message) -> Result<(), ViewerError> {
        let sender = self.state.control().ok_or(ViewerError::NotAuthenticated)?;
        sender.try_send(message).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => ViewerError::Backpressure,
            mpsc::error::TrySendError::Closed(_) => ViewerError::Closed,
        })
    }

    /// x/y are selected-display-local pixels for absolute events; the original
    /// protocol receives desktop-global coordinates. Wheel/trackpad/relative
    /// events retain signed deltas. Original mask = kind | (button_bits << 3).
    pub fn send_mouse(&self, kind: u32, button: u32, x: i32, y: i32) -> Result<(), ViewerError> {
        if !LOCAL_MOUSE_ENABLED
            || !self.state.authenticated.load(Ordering::Acquire)
            || !self.state.remote_keyboard.load(Ordering::Acquire)
            || self.state.cancel.is_cancelled()
        {
            return Err(ViewerError::InputNotAllowed);
        }
        let valid = match kind {
            0 => button <= 0x1f,
            1 | 2 => matches!(button, 1 | 2 | 4 | 8 | 16),
            3..=5 => button == 0,
            _ => false,
        };
        if !valid {
            return Err(ViewerError::InvalidMouse);
        }
        let (x, y) = if kind <= 2 {
            let geometry = lock(&self.state.inner)
                .geometry
                .ok_or(ViewerError::InputNotAllowed)?;
            if x < 0 || y < 0 || x >= geometry.width || y >= geometry.height {
                return Err(ViewerError::InvalidMouse);
            }
            (
                x.checked_add(geometry.x).ok_or(ViewerError::InvalidMouse)?,
                y.checked_add(geometry.y).ok_or(ViewerError::InvalidMouse)?,
            )
        } else {
            (x, y)
        };
        let mut message = Message::new();
        message.set_mouse_event(MouseEvent {
            mask: (kind | (button << 3)) as i32,
            x,
            y,
            ..Default::default()
        });
        self.mouse.try_send(message).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => ViewerError::Backpressure,
            mpsc::error::TrySendError::Closed(_) => ViewerError::Closed,
        })
    }
    pub fn request_close(&self) {
        self.state.request_close();
    }
    /// Confirms resource reclamation only. Authentication/network/media errors
    /// stay in snapshot.error; they do not prevent a safe retry on this Surface.
    pub async fn close(&self) -> Result<(), ViewerError> {
        self.request_close();
        loop {
            let done = self.state.done.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if let Some(result) = lock(&self.state.result).clone() {
                return result;
            }
            done.await;
        }
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.state.request_close();
    }
}

struct Password(Vec<u8>);
impl Drop for Password {
    fn drop(&mut self) {
        hbb_common::sodiumoxide::utils::memzero(&mut self.0);
    }
}
#[derive(Clone, Copy)]
struct Decoders {
    h264: bool,
    h265: bool,
}
fn advertised(value: &Result<AdvertisedCodec, CapabilityError>) -> bool {
    value.as_ref().is_ok_and(|v| {
        v.hardware
            && !v.codec_name.is_empty()
            && v.native_buffer_formats
                .as_ref()
                .is_ok_and(|f| !f.is_empty())
    })
}
fn capabilities() -> Result<Decoders, ViewerError> {
    let h264 = media_capability::query_h264_hardware_decoder();
    let h265 = media_capability::query_hevc_hardware_capabilities().decoder;
    if matches!(&h264, Err(CapabilityError::UnsupportedPlatform))
        && matches!(&h265, Err(CapabilityError::UnsupportedPlatform))
    {
        return Err(ViewerError::UnsupportedPlatform);
    }
    let result = Decoders {
        h264: advertised(&h264),
        h265: advertised(&h265),
    };
    if !result.h264 && !result.h265 {
        return Err(ViewerError::NoHardwareCodec);
    }
    Ok(result)
}
fn login_request(options: &ViewerOptions, decoders: Decoders) -> LoginRequest {
    // Key handshake has initialized sodium before Challenge. Never reuse a
    // process-local counter as an original-peer reconnect session identifier.
    let mut session_bytes = [0u8; 8];
    hbb_common::sodiumoxide::randombytes::randombytes_into(&mut session_bytes);
    let supported = SupportedDecoding {
        ability_h264: i32::from(decoders.h264),
        ability_h265: i32::from(decoders.h265),
        // No VPx/AV1 baseline assumption. These zeroes are deliberate.
        ability_vp9: 0,
        ability_vp8: 0,
        ability_av1: 0,
        prefer: if decoders.h265 {
            PreferCodec::H265
        } else {
            PreferCodec::H264
        }
        .into(),
        prefer_chroma: proto::Chroma::I420.into(),
        ..Default::default()
    };
    LoginRequest {
        username: options.username.clone(),
        my_id: options.local_id.clone(),
        my_name: options.local_name.clone(),
        my_platform: "HarmonyOS".into(),
        version: "1.4.9".into(),
        session_id: u64::from_le_bytes(session_bytes).max(1),
        video_ack_required: false,
        option: Some(OptionMessage {
            supported_decoding: Some(supported).into(),
            custom_fps: options.requested_fps as i32,
            image_quality: options.image_quality.proto_quality().into(),
            custom_image_quality: options.image_quality.custom_quality().unwrap_or_default(),
            disable_audio: BoolOption::Yes.into(),
            disable_clipboard: if LOCAL_CLIPBOARD_ENABLED && options.clipboard_enabled {
                BoolOption::No
            } else {
                BoolOption::Yes
            }
            .into(),
            enable_file_transfer: BoolOption::No.into(),
            disable_keyboard: if LOCAL_KEYBOARD_ENABLED {
                BoolOption::No
            } else {
                BoolOption::Yes
            }
            .into(),
            disable_camera: BoolOption::Yes.into(),
            ..Default::default()
        })
        .into(),
        ..Default::default()
    }
}

fn geometry(info: &PeerInfo) -> Result<Geometry, ViewerError> {
    let index =
        usize::try_from(info.current_display).map_err(|_| ViewerError::InvalidPeerGeometry)?;
    let display = info
        .displays
        .get(index)
        .ok_or(ViewerError::InvalidPeerGeometry)?;
    if display.width <= 0 || display.height <= 0 {
        return Err(ViewerError::InvalidPeerGeometry);
    }
    Ok(Geometry {
        display: info.current_display,
        x: display.x,
        y: display.y,
        width: display.width,
        height: display.height,
    })
}

async fn authenticate(
    state: &State,
    mut options: ViewerOptions,
    decoders: Decoders,
    mut authentication: mpsc::Receiver<AuthCommand>,
    connector: Connector,
) -> Result<AuthenticatedParts, ViewerError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Awaiting {
        Network,
        Password,
        SecondFactor,
    }
    enum Incoming {
        Network(ViewerEvent),
        Command(AuthCommand),
    }

    let password = Password(std::mem::take(&mut options.password).into_bytes());
    let mut request = None;
    let mut awaiting = Awaiting::Network;
    let (mut session, insecure_confirmation_required) = match connector {
        Connector::Direct(identity) => {
            state.phase("connecting");
            let address = options.address.ok_or(ViewerError::InvalidOptions)?;
            let insecure_confirmation_required =
                matches!(identity, ViewerIdentity::LegacyUnverified);
            let session = ViewerSession::connect_direct(address, identity, AUTH_TIMEOUT)
                .await
                .map_err(|_| ViewerError::ConnectFailed)?;
            state.route("direct_tcp");
            (session, insecure_confirmation_required)
        }
        Connector::Rendezvous(config) => {
            state.phase("rendezvous");
            let timeout = config.connect_timeout;
            let (established, evidence) = crate::rendezvous::connect_viewer(config)
                .await
                .map_err(|_| ViewerError::RendezvousFailed)?;
            if !evidence.encrypted || !evidence.peer_verified {
                return Err(ViewerError::RendezvousFailed);
            }
            state.route(match evidence.route {
                RouteKind::TcpHolePunch => "tcp_hole_punch",
                RouteKind::Relay => "relay",
            });
            (
                ViewerSession::from_established(established, timeout, AUTH_TIMEOUT),
                false,
            )
        }
    };
    if insecure_confirmation_required {
        state.phase("awaiting_insecure_confirmation");
        loop {
            match authentication.recv().await.ok_or(ViewerError::Closed)? {
                AuthCommand::ContinueInsecure(true) => break,
                AuthCommand::ContinueInsecure(false) => {
                    return Err(ViewerError::AuthenticationFailed);
                }
                // Password/2FA cannot bypass the explicit transport decision.
                AuthCommand::Password(_) | AuthCommand::SecondFactor(_) => {}
            }
        }
    }
    state.phase("authenticating");
    loop {
        let incoming = if awaiting == Awaiting::Network {
            Incoming::Network(
                session
                    .recv()
                    .await
                    .map_err(|_| ViewerError::AuthenticationFailed)?,
            )
        } else {
            tokio::select! {
                event = session.recv() => Incoming::Network(
                    event.map_err(|_| ViewerError::AuthenticationFailed)?
                ),
                command = authentication.recv() => Incoming::Command(
                    command.ok_or(ViewerError::Closed)?
                ),
            }
        };
        let event = match incoming {
            Incoming::Command(AuthCommand::Password(password))
                if awaiting == Awaiting::Password =>
            {
                let request = request
                    .as_ref()
                    .cloned()
                    .ok_or(ViewerError::AuthenticationFailed)?;
                session
                    .login(request, Some(&password.0))
                    .await
                    .map_err(|_| ViewerError::AuthenticationFailed)?;
                state.phase("authenticating");
                awaiting = Awaiting::Network;
                continue;
            }
            Incoming::Command(AuthCommand::SecondFactor(mut code))
                if awaiting == Awaiting::SecondFactor =>
            {
                let code = String::from_utf8(std::mem::take(&mut code.0))
                    .map_err(|_| ViewerError::InvalidOptions)?;
                session
                    .send_second_factor(code)
                    .await
                    .map_err(|_| ViewerError::AuthenticationFailed)?;
                state.phase("authenticating");
                awaiting = Awaiting::Network;
                continue;
            }
            Incoming::Command(AuthCommand::ContinueInsecure(_)) => continue,
            Incoming::Command(_) => continue,
            Incoming::Network(event) => event,
        };
        match event {
            ViewerEvent::Challenge => {
                let first = login_request(&options, decoders);
                session
                    .login(
                        first.clone(),
                        if password.0.is_empty() {
                            None
                        } else {
                            Some(&password.0)
                        },
                    )
                    .await
                    .map_err(|_| ViewerError::AuthenticationFailed)?;
                request = Some(first);
            }
            ViewerEvent::Authorized(_) => {
                return session
                    .into_authenticated_parts()
                    .map_err(|_| ViewerError::AuthenticationFailed);
            }
            ViewerEvent::LoginError(error) if error == "No Password Access" => {
                state.phase("awaiting_approval");
                awaiting = Awaiting::Network;
            }
            ViewerEvent::LoginError(error) if error == "Wrong Password" => {
                state.phase("awaiting_password");
                awaiting = Awaiting::Password;
            }
            ViewerEvent::LoginError(error)
                if error == "2FA Required" || error == "Wrong 2FA Code" =>
            {
                state.phase(if error == "Wrong 2FA Code" {
                    "awaiting_2fa_retry"
                } else {
                    "awaiting_2fa"
                });
                awaiting = Awaiting::SecondFactor;
            }
            ViewerEvent::LoginError(_) => return Err(ViewerError::AuthenticationFailed),
            ViewerEvent::Closed => return Err(ViewerError::RemoteClosed),
            ViewerEvent::Progress | ViewerEvent::PreAuthControl(_) => {}
            _ => return Err(ViewerError::AuthenticationFailed),
        }
    }
}

struct VideoRecord {
    codec: VideoCodec,
    geometry: Geometry,
    frames: Vec<EncodedVideoFrame>,
    // Held until the ordered feeder finishes this group, not released by recv.
    _slot: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

async fn run(
    state: Arc<State>,
    options: ViewerOptions,
    lease: Arc<dyn SurfaceLease>,
    mouse: mpsc::Receiver<Message>,
    authentication: mpsc::Receiver<AuthCommand>,
    connector: Connector,
) -> Result<(), ViewerError> {
    // Metadata IPC is also off the UI / network worker. Do not discard a native
    // creation JoinHandle on cancellation; the feeder always joins and closes it.
    let caps = tokio::task::spawn_blocking(capabilities)
        .await
        .map_err(|_| ViewerError::TaskFailed)??;
    let parts = tokio::select! {
        biased;
        _ = state.cancel.cancelled() => return Ok(()),
        parts = authenticate(&state, options, caps, authentication, connector) => parts?,
    };
    {
        let mut inner = state.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.snapshot.encrypted = matches!(
            &parts.context.security,
            crate::handshake::Security::Encrypted { .. }
        );
        inner.snapshot.peer_verified = matches!(
            &parts.context.security,
            crate::handshake::Security::Encrypted { peer_id: Some(_) }
        );
    }
    let source = geometry(
        parts
            .context
            .peer_info
            .as_ref()
            .ok_or(ViewerError::InvalidPeerGeometry)?,
    )?;
    // Store the accepted peer report verbatim; consumers read it read-only.
    *state
        .peer_info
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = parts.context.peer_info.clone();
    state.geometry(source);
    state
        .remote_keyboard
        .store(parts.context.permissions.keyboard, Ordering::Release);
    state
        .remote_clipboard
        .store(parts.context.permissions.clipboard, Ordering::Release);
    state.authenticated.store(true, Ordering::Release);
    state.phase("authenticated");
    let (video_tx, video_rx) = mpsc::channel(VIDEO_RECORDS);
    let (control_tx, control_rx) = mpsc::channel(COMMANDS);
    // Publish the control sender only now: before this point no message may be
    // written on a connection that has not completed the login handshake.
    *state
        .control
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(control_tx.clone());
    let mut tasks = JoinSet::new();
    tasks.spawn(receive(
        state.clone(),
        parts.reader,
        source,
        caps,
        video_tx,
        control_tx,
    ));
    tasks.spawn(write(state.clone(), parts.writer, mouse, control_rx));
    tasks.spawn(feed(state.clone(), video_rx, lease));
    let first = tokio::select! {
        biased;
        _ = state.cancel.cancelled() => Ok(()),
        result = tasks.join_next() => match result {
            Some(Ok(result)) => result,
            _ => Err(ViewerError::TaskFailed),
        },
    };
    state.request_close();
    let mut result = first;
    // Never abort the feeder: it owns the native decoder and its blocking join.
    while let Some(joined) = tasks.join_next().await {
        let next = joined.unwrap_or(Err(ViewerError::TaskFailed));
        if let Err(error) = next {
            // A teardown failure must reach HAR even when localClose was first.
            if result.is_ok()
                || matches!(
                    &error,
                    ViewerError::Decoder(DecoderError::DestroyFailed { .. })
                )
            {
                result = Err(error);
            }
        }
    }
    // Revoke the control sender before reporting completion so a caller racing
    // teardown cannot enqueue onto a finished session.
    *state
        .control
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    result
}

async fn receive(
    state: Arc<State>,
    mut reader: WireReader,
    mut source: Geometry,
    caps: Decoders,
    video: mpsc::Sender<VideoRecord>,
    control: mpsc::Sender<Message>,
) -> Result<(), ViewerError> {
    let slots = Arc::new(Semaphore::new(VIDEO_RECORDS));
    let bytes = Arc::new(Semaphore::new(VIDEO_BYTES));
    loop {
        let message = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            result = reader.recv() => result.map_err(|_| ViewerError::TransportFailed)?.ok_or(ViewerError::RemoteClosed)?,
        };
        match message.union {
            Some(message::Union::VideoFrame(frame)) => {
                if frame.display != source.display {
                    return Err(ViewerError::InvalidPeerGeometry);
                }
                let (codec, frames) = match frame.union {
                    Some(video_frame::Union::H264s(frames)) if caps.h264 => {
                        (VideoCodec::H264, frames.frames)
                    }
                    Some(video_frame::Union::H265s(frames)) if caps.h265 => {
                        (VideoCodec::H265, frames.frames)
                    }
                    _ => return Err(ViewerError::UnsupportedCodec),
                };
                if frames.is_empty() {
                    continue;
                }
                if frames.len() > MAX_UNITS_PER_RECORD || frames.iter().any(|f| f.data.is_empty()) {
                    return Err(ViewerError::InvalidVideoRecord);
                }
                let length = frames
                    .iter()
                    .try_fold(0usize, |n, f| n.checked_add(f.data.len()))
                    .ok_or(ViewerError::VideoRecordTooLarge)?;
                if length > VIDEO_BYTES {
                    return Err(ViewerError::VideoRecordTooLarge);
                }
                lock(&state.inner).snapshot.received_units += frames.len() as u64;
                // Transport itself bounds the one not-yet-admitted RX record to
                // 64MiB. Admitted records INCLUDING the feeder are byte+slot bound.
                let record = tokio::select! {
                    biased;
                    _ = state.cancel.cancelled() => return Ok(()),
                    admitted = async {
                        let slot = slots.clone().acquire_owned().await.map_err(|_| ViewerError::Closed)?;
                        let permit = bytes.clone().acquire_many_owned(length as u32).await.map_err(|_| ViewerError::Closed)?;
                        Ok::<_, ViewerError>(VideoRecord { codec, geometry: source, frames, _slot: slot, _bytes: permit })
                    } => admitted?,
                };
                tokio::select! {
                    biased;
                    _ = state.cancel.cancelled() => return Ok(()),
                    sent = video.send(record) => sent.map_err(|_| ViewerError::Closed)?,
                }
            }
            Some(message::Union::TestDelay(probe)) => {
                if !probe.from_client {
                    let mut response = Message::new();
                    response.set_test_delay(probe);
                    control
                        .try_send(response)
                        .map_err(|_| ViewerError::Backpressure)?;
                }
            }
            Some(message::Union::Misc(m)) => match m.union {
                Some(misc::Union::PermissionInfo(permission)) => {
                    match permission.permission.enum_value().ok() {
                        Some(Permission::Keyboard) => state
                            .remote_keyboard
                            .store(permission.enabled, Ordering::Release),
                        Some(Permission::Clipboard) => state
                            .remote_clipboard
                            .store(permission.enabled, Ordering::Release),
                        _ => {}
                    }
                }
                Some(misc::Union::CloseReason(_)) => return Err(ViewerError::RemoteClosed),
                Some(misc::Union::SwitchDisplay(display)) => {
                    if display.width <= 0 || display.height <= 0 || display.display < 0 {
                        return Err(ViewerError::InvalidPeerGeometry);
                    }
                    source = Geometry {
                        display: display.display,
                        x: display.x,
                        y: display.y,
                        width: display.width,
                        height: display.height,
                    };
                    // The peer owns display geometry; publish it and tell the
                    // UI so the viewport re-fits the authoritative source.
                    state.geometry(source);
                }
                Some(misc::Union::Option(option)) => {
                    if let Ok(quality) = option.image_quality.enum_value() {
                        let next = match quality {
                            proto::ImageQuality::Low => Some(ViewerImageQuality::Low),
                            proto::ImageQuality::Balanced => Some(ViewerImageQuality::Balanced),
                            proto::ImageQuality::Best => Some(ViewerImageQuality::Best),
                            proto::ImageQuality::NotSet => None,
                        };
                        if let Some(next) = next {
                            *state
                                .image_quality
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = next;
                        }
                    }
                    if option.custom_image_quality > 0 {
                        *state
                            .image_quality
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) =
                            ViewerImageQuality::Custom(option.custom_image_quality);
                    }
                    if option.custom_fps > 0 {
                        state
                            .requested_fps
                            .store(option.custom_fps as u32, Ordering::Release);
                    }
                }
                _ => {}
            },
            Some(message::Union::Clipboard(clipboard)) => {
                store_inbound_clipboard(&state, std::slice::from_ref(&clipboard));
            }
            Some(message::Union::MultiClipboards(clipboards)) => {
                store_inbound_clipboard(&state, &clipboards.clipboards);
            }
            // Audio/file features are explicitly disabled and cursor messages do
            // not authorize input or fabricate video state.
            _ => {}
        }
    }
}

fn decompress_with_limit(data: &[u8], limit: usize) -> std::io::Result<Vec<u8>> {
    let decoder = zstd::Decoder::new(data)?;
    let mut output = Vec::new();
    decoder
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut output)?;
    if output.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "decompressed data exceeds size limit",
        ));
    }
    Ok(output)
}

/// Accept only text clipboards, bounded after decompression. Anything else is
/// dropped rather than stored in an unvalidated form.
fn store_inbound_clipboard(state: &State, clipboards: &[Clipboard]) {
    if !LOCAL_CLIPBOARD_ENABLED || !state.local_clipboard {
        return;
    }
    for clipboard in clipboards {
        if clipboard.format.enum_value().ok() != Some(ClipboardFormat::Text) {
            continue;
        }
        if clipboard.content.len() > MAX_INBOUND_CLIPBOARD_TEXT {
            continue;
        }
        let raw = if clipboard.compress {
            match decompress_with_limit(&clipboard.content, MAX_INBOUND_CLIPBOARD_TEXT) {
                Ok(raw) => raw,
                Err(_) => continue,
            }
        } else {
            clipboard.content.to_vec()
        };
        if raw.is_empty() || raw.len() > MAX_INBOUND_CLIPBOARD_TEXT {
            continue;
        }
        let Ok(text) = String::from_utf8(raw) else {
            continue;
        };
        let mut inner = lock(&state.inner);
        if inner.clipboard.len() >= CLIPBOARD_QUEUE {
            inner.clipboard.remove(0);
        }
        inner.clipboard.push_back(text);
    }
}

async fn write(
    state: Arc<State>,
    mut writer: WireWriter,
    mut mouse: mpsc::Receiver<Message>,
    mut control: mpsc::Receiver<Message>,
) -> Result<(), ViewerError> {
    loop {
        let (message, input) = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            value = control.recv() => match value { Some(m) => (m, false), None => return Ok(()) },
            value = mouse.recv() => match value { Some(m) => (m, true), None => return Ok(()) },
        };
        // Recheck revocation at SEND time, not only UI enqueue time.
        if input
            && (!LOCAL_MOUSE_ENABLED
                || !state.authenticated.load(Ordering::Acquire)
                || !state.remote_keyboard.load(Ordering::Acquire))
        {
            continue;
        }
        tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            result = writer.send(&message) => result.map_err(|_| ViewerError::TransportFailed)?,
        }
    }
}

async fn close_decoder(
    state: &State,
    current: &mut Option<SurfaceDecoder>,
) -> Result<(), ViewerError> {
    let Some(decoder) = current.take() else {
        return Ok(());
    };
    let observer = decoder.observer();
    let closed = tokio::task::spawn_blocking(move || decoder.close())
        .await
        .map_err(|_| ViewerError::TaskFailed);
    let stats = observer.stats();
    if stats
        .as_ref()
        .is_ok_and(|stats| stats.closed && !stats.quarantined)
    {
        // close() can carry an earlier codec/Stop error even though Destroy
        // succeeded. The worker's final ownership state is the reclamation proof.
        state.resources_unconfirmed.store(false, Ordering::Release);
    }
    let mut inner = lock(&state.inner);
    if let Ok(stats) = stats {
        inner.snapshot.pushed_units = inner
            .snapshot
            .pushed_units
            .saturating_add(stats.pushed_units);
        inner.snapshot.render_submissions = inner
            .snapshot
            .render_submissions
            .saturating_add(stats.render_submissions);
    }
    inner.decoder = None;
    closed?.map(|_| ()).map_err(ViewerError::Decoder)
}

async fn feed(
    state: Arc<State>,
    mut video: mpsc::Receiver<VideoRecord>,
    lease: Arc<dyn SurfaceLease>,
) -> Result<(), ViewerError> {
    let mut current = None;
    let result = feed_loop(&state, &mut video, lease, &mut current).await;
    if result.is_err() {
        // Network/UI cancellation must not wait for a blocking SDK teardown.
        state.request_close();
    }
    let teardown = close_decoder(&state, &mut current).await;
    teardown?;
    result
}

async fn feed_loop(
    state: &State,
    video: &mut mpsc::Receiver<VideoRecord>,
    lease: Arc<dyn SurfaceLease>,
    current: &mut Option<SurfaceDecoder>,
) -> Result<(), ViewerError> {
    let mut contract = None;
    loop {
        let observer = current.as_ref().map(SurfaceDecoder::observer);
        let record = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            stopped = async {
                match observer { Some(observer) => observer.wait_stopped().await, None => std::future::pending().await }
            } => return Err(ViewerError::Decoder(stopped.err().unwrap_or(DecoderError::Closed))),
            record = video.recv() => match record { Some(record) => record, None => return Ok(()) },
        };
        let next = (record.codec, record.geometry);
        if contract != Some(next) {
            close_decoder(state, current).await?;
            if state.cancel.is_cancelled() {
                return Ok(());
            }
            state.phase("opening_decoder");
            let lease = lease.clone();
            state.resources_unconfirmed.store(true, Ordering::Release);
            // Read before the closure takes ownership: the decoder is built on a
            // blocking thread, and the rate is a plain value.
            let stream_frame_rate = state.requested_fps.load(Ordering::Acquire);
            let opened = tokio::task::spawn_blocking(move || {
                SurfaceDecoder::open(
                    DecoderConfig {
                        codec: next.0,
                        width: next.1.width,
                        height: next.1.height,
                        // The rate the peer was asked to encode at. The platform's
                        // variable refresh rate uses it to drive the panel, so it
                        // has to be the stream's real rate and not a default.
                        frame_rate: stream_frame_rate,
                        // One AU may be inside PushInputBuffer while one waits
                        // for an input callback; do not build a stale decode tail.
                        max_queued_units: 2,
                        max_queued_bytes: VIDEO_BYTES,
                    },
                    lease,
                )
            })
            .await
            .map_err(|_| ViewerError::TaskFailed)?;
            let decoder = match opened {
                Ok(decoder) => decoder,
                Err(error) => {
                    // open() joins partial initialization cleanup before return.
                    // Only Destroy failure / worker unwind leaves native ownership
                    // unproven. OwnerLimit/QuarantinePresent allocate no decoder.
                    if !matches!(
                        error,
                        DecoderError::DestroyFailed { .. } | DecoderError::WorkerPanicked
                    ) {
                        state.resources_unconfirmed.store(false, Ordering::Release);
                    }
                    return Err(ViewerError::Decoder(error));
                }
            };
            // Keep ownership even if close arrived during the blocking open; the
            // outer feeder will always run blocking teardown, never drop on IO.
            *current = Some(decoder);
            state.geometry(next.1);
            let mut inner = lock(&state.inner);
            inner.decoder = current.as_ref().map(SurfaceDecoder::observer);
            inner.snapshot.codec = match next.0 {
                VideoCodec::H264 => "H264",
                VideoCodec::H265 => "H265",
            }
            .into();
            inner.snapshot.phase = "decoding".into();
            contract = Some(next);
        }
        let decoder = current.as_ref().ok_or(ViewerError::Closed)?;
        // Keep both record permits until its complete FIFO group has crossed
        // decoder admission. Each AU also retains decoder in-progress quota.
        for frame in record.frames {
            if state.cancel.is_cancelled() {
                return Ok(());
            }
            let pts_us = frame
                .pts
                .checked_mul(1000)
                .ok_or(ViewerError::TimestampOverflow)?;
            let unit = AccessUnit {
                bytes: frame.data.to_vec(),
                pts_us,
                kind: AccessUnitKind::Frame { key: frame.key },
                // Stamped by the decoder at admission, which is the point the
                // pipeline measurement starts from.
                accepted_at: None,
                // Stamped by the decoder when the unit leaves its queue.
                dequeued_at: None,
            };
            if let Err(rejected) = decoder.submit_cancellable(unit, &state.cancel).await {
                if state.cancel.is_cancelled() {
                    return Ok(());
                }
                return Err(ViewerError::Decoder(rejected.reason));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_decompression_rejects_oversize_clipboards() {
        let compressed = zstd::encode_all(&vec![0u8; 17][..], 0).unwrap();
        assert!(decompress_with_limit(&compressed, 16).is_err());
    }

    #[test]
    fn bounded_decompression_accepts_the_exact_limit() {
        let input = vec![7u8; 16];
        let compressed = zstd::encode_all(&input[..], 0).unwrap();
        assert_eq!(
            decompress_with_limit(&compressed, input.len()).unwrap(),
            input
        );
    }

    #[test]
    fn legacy_key_names_cover_single_characters_and_protocol_control_keys() {
        // A single character is the raw `chr` code, exactly like the original
        // client's KEY_MAP fallback; it is never promoted to a control key.
        assert_eq!(legacy_key_name("v"), Some(ViewerKey::Character('v' as u32)));
        assert_eq!(legacy_key_name("V"), Some(ViewerKey::Character('V' as u32)));
        assert_eq!(
            legacy_key_name("VK_RETURN"),
            Some(ViewerKey::Control(ControlKey::Return))
        );
        assert_eq!(
            legacy_key_name("Return"),
            Some(ViewerKey::Control(ControlKey::Return))
        );
        assert_eq!(
            legacy_key_name("LOCK_SCREEN"),
            Some(ViewerKey::Control(ControlKey::LockScreen))
        );
        assert_eq!(legacy_key_name(""), None);
        assert_eq!(legacy_key_name("NOT_A_KEY"), None);
    }

    #[test]
    fn usb_hid_mapping_prefers_event_character_then_us_layout() {
        // The event character wins so the peer's own layout resolution applies.
        assert_eq!(
            usb_hid_key(0x14, "q"),
            Some(ViewerKey::Character('q' as u32))
        );
        assert_eq!(
            usb_hid_key(0x1e, "!"),
            Some(ViewerKey::Character('!' as u32))
        );
        // Non-printable usage codes are protocol control keys.
        assert_eq!(
            usb_hid_key(0x28, ""),
            Some(ViewerKey::Control(ControlKey::Return))
        );
        assert_eq!(
            usb_hid_key(0x3a, ""),
            Some(ViewerKey::Control(ControlKey::F1))
        );
        assert_eq!(
            usb_hid_key(0xe3, ""),
            Some(ViewerKey::Control(ControlKey::Meta))
        );
        assert_eq!(
            usb_hid_key(0x59, ""),
            Some(ViewerKey::Control(ControlKey::Numpad1))
        );
        // No character and no control mapping: US fallback for printable keys.
        assert_eq!(
            usb_hid_key(0x04, ""),
            Some(ViewerKey::Character('a' as u32))
        );
        assert_eq!(
            usb_hid_key(0x2d, ""),
            Some(ViewerKey::Character('-' as u32))
        );
        // An unmapped usage code is rejected rather than guessed.
        assert_eq!(usb_hid_key(0x00, ""), None);
        assert_eq!(usb_hid_key(0x83, ""), None);
    }

    #[test]
    fn image_quality_never_reports_a_preset_as_custom() {
        assert_eq!(ViewerImageQuality::Low.label(), "low");
        assert_eq!(ViewerImageQuality::Best.label(), "best");
        // Out-of-range custom values are refused instead of being clamped.
        assert_eq!(
            ViewerImageQuality::Custom(5000).label(),
            "custom",
            "the label stays custom; validation happens at the API boundary"
        );
        assert_eq!(ViewerImageQuality::Custom(5000).custom_quality(), None);
        assert_eq!(ViewerImageQuality::Custom(50).custom_quality(), Some(50));
        assert_eq!(
            ViewerImageQuality::Balanced.proto_quality(),
            proto::ImageQuality::Balanced
        );
    }

    #[test]
    fn legacy_modifiers_follow_the_protocol_order() {
        let mut event = KeyEvent::new();
        legacy_modifiers(&mut event, true, true, true, true);
        let values: Vec<ControlKey> = event
            .modifiers
            .iter()
            .filter_map(|modifier| modifier.enum_value().ok())
            .collect();
        assert_eq!(
            values,
            vec![
                ControlKey::Alt,
                ControlKey::Shift,
                ControlKey::Control,
                ControlKey::Meta
            ]
        );
    }
}
