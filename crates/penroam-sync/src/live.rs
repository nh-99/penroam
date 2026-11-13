//! The live channel of a shared notebook space (`live.v1`, protocol §12g): commit hints, who is
//! where in the notebook (awareness), and membership changes, over a WebSocket.
//!
//! **The live channel never writes.** Everything durable still goes through the operation log
//! and HTTP sync. A `committed { server_seq }` hint only tells the client to pull now instead of
//! at the next timer; a hint that is lost, duplicated or late costs latency, never data, and the
//! client always catches up over HTTP after (re)connecting ([`LiveEvent::CatchUp`]).
//!
//! [`LiveClient`] is the connection's state machine, independent of the socket: a [`LiveDialer`]
//! opens a [`LiveLink`] (a WebSocket in the app, the test server's hub in tests). The app runs one
//! per active space on its own thread and calls [`LiveClient::step`] in a loop:
//!
//! * not connected → dial when the backoff allows (full jitter, 1 s doubling to 60 s), then send
//!   `hello` with the last known server sequence; `welcome` → [`LiveState::Live`];
//! * live → ping every heartbeat, give up on a silent socket after two heartbeats, track peers
//!   from awareness frames (each expires after its TTL);
//! * access removed (an HTTP refusal or `bye access_revoked` or a `membership` saying revoked) →
//!   [`LiveEvent::AccessRemoved`] and the client stops for good ([`LiveState::Disabled`]);
//!   `upgrade_required` stops it too.
//!
//! Awareness payloads are sealed (XChaCha20-Poly1305) under the space's awareness key
//! (`RootSecret::awareness_key`, HKDF of the space root), with the space id and the sender's
//! device id as associated data, so the server relays them without reading them, and a frame
//! moved to another space or sender fails to open. Outgoing awareness is limited to 15 frames a
//! second and to [`MAX_AWARENESS_PLAINTEXT_BYTES`] of JSON.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use penroam_sync_protocol::live::{
    awareness_aad, Awareness, Bye, ByeCode, ClientFrame, Hello, ServerFrame, Welcome,
    MAX_AWARENESS_PLAINTEXT_BYTES,
};
use penroam_sync_protocol::version::ProtocolVersion;
use penroam_sync_protocol::{
    base64url, AccountId, DeviceId, ErrorBody, ErrorCode, MembershipState, Role,
};

use crate::crypto::{self, KEY_LEN};

/// Outgoing awareness frames per second (and burst).
pub const AWARENESS_SEND_PER_S: f64 = 15.0;
/// How long an outgoing awareness frame stays valid, in milliseconds.
pub const AWARENESS_TTL_MS: u32 = 10_000;
const BACKOFF_BASE_MS: u64 = 1_000;
/// Most peers tracked at once; awareness from further sessions is ignored.
pub const MAX_PEERS: usize = 64;
const BACKOFF_CAP_MS: u64 = 60_000;

/// Where the live channel of one space stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LiveState {
    /// Dialing (or waiting to dial again).
    Connecting,
    /// Connected; hints and awareness flow.
    Live,
    /// Not connected; ordinary sync goes on and the client retries.
    Offline,
    /// Stopped for good: access removed, an update needed, or turned off.
    Disabled,
}

/// What a step of [`LiveClient`] reports.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveEvent {
    /// The state changed.
    State(LiveState),
    /// Pull this space over HTTP now: a commit hint arrived, or the channel just (re)connected.
    /// Carries the server sequence the hint or welcome named.
    CatchUp(u64),
    /// Pull this space's comment stream now (a comment was committed).
    CommentsCatchUp(u64),
    /// A notification for this account was created: refresh the notification list.
    Notification,
    /// The set of peers (other sessions with unexpired awareness) changed.
    Peers(Vec<Peer>),
    /// The server says a member's role or state changed.
    Membership {
        /// The member.
        account: AccountId,
        /// Their role now.
        role: Role,
        /// Their membership state now.
        state: MembershipState,
    },
    /// The server says this account lost access to the space; the client stopped. This is only a
    /// cue: the live channel isn't authenticated the way HTTP is (a notice could come from another
    /// server instance), so the app confirms over HTTP (the shared notebook list, or an
    /// `access_revoked` answer) before treating the notebook as removed.
    AccessRemoved,
}

/// Another session in the space, from its latest awareness frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Peer {
    /// The other session's id.
    pub session_id: String,
    /// The member account the server verified for it.
    pub member: AccountId,
    /// Its decrypted awareness JSON.
    pub payload: serde_json::Value,
    /// When it expires (`Instant` of this process).
    pub expires: Instant,
}

/// Why dialing failed.
#[derive(Debug, Clone, PartialEq)]
pub enum DialError {
    /// The server refused the upgrade with an HTTP error.
    Refused(ErrorBody),
    /// The server answered the hello with `bye`.
    Bye(Bye),
    /// The network (or anything else worth retrying).
    Network(String),
}

/// One open live connection.
pub trait LiveLink: Send {
    /// Sends a frame.
    fn send(&mut self, frame: &ClientFrame) -> Result<(), String>;
    /// Waits up to `wait` for the next frame; `Ok(None)` when nothing arrived. `Err` means the
    /// connection is gone.
    fn recv(&mut self, wait: Duration) -> Result<Option<ServerFrame>, String>;
    /// Closes the connection.
    fn close(&mut self);
}

/// Opens live connections.
pub trait LiveDialer: Send {
    /// Connects and exchanges `hello`/`welcome`.
    fn dial(&mut self, hello: &Hello) -> Result<(Welcome, Box<dyn LiveLink>), DialError>;
}

/// The live channel of one space (see the module docs).
pub struct LiveClient {
    space: AccountId,
    member: AccountId,
    device: DeviceId,
    key: [u8; KEY_LEN],
    presence: bool,
    dialer: Box<dyn LiveDialer>,
    link: Option<Box<dyn LiveLink>>,
    welcome: Option<Welcome>,
    state: LiveState,
    attempt: u32,
    next_dial: Instant,
    last_rx: Instant,
    last_ping: Instant,
    peers: BTreeMap<String, Peer>,
    tokens: f64,
    tokens_at: Instant,
    last_server_seq: u64,
    /// When the current link was established (the backoff resets only once it has lasted).
    connected_at: Option<Instant>,
}

impl LiveClient {
    /// A client for `space` as `member`'s `device`, sealing awareness under `awareness_key`
    /// (`RootSecret::awareness_key` of the space's root). `presence` = whether this person
    /// shares their presence (the app preference); hints arrive either way.
    pub fn new(
        space: AccountId,
        member: AccountId,
        device: DeviceId,
        awareness_key: [u8; KEY_LEN],
        presence: bool,
        dialer: Box<dyn LiveDialer>,
    ) -> Self {
        let now = Instant::now();
        Self {
            space,
            member,
            device,
            key: awareness_key,
            presence,
            dialer,
            link: None,
            welcome: None,
            state: LiveState::Connecting,
            attempt: 0,
            next_dial: now,
            last_rx: now,
            last_ping: now,
            peers: BTreeMap::new(),
            tokens: AWARENESS_SEND_PER_S,
            tokens_at: now,
            last_server_seq: 0,
            connected_at: None,
        }
    }

    /// The current state.
    pub fn state(&self) -> LiveState {
        self.state
    }

    /// Consecutive failed connections behind the current backoff (tests and diagnostics).
    #[doc(hidden)]
    pub fn backoff_attempt(&self) -> u32 {
        self.attempt
    }

    /// The live session id, while connected.
    pub fn session_id(&self) -> Option<&str> {
        self.welcome.as_ref().map(|w| w.session_id.as_str())
    }

    /// Turns sharing presence on or off. Takes effect at the next connection (the server learns
    /// it from `hello`); while off, nothing is sent.
    pub fn set_presence(&mut self, on: bool) {
        if self.presence != on {
            self.presence = on;
            // reconnect so the server stops (or starts) counting this session as present
            self.drop_link(Instant::now(), false);
        }
    }

    /// Stops for good.
    pub fn stop(&mut self) -> Vec<LiveEvent> {
        if let Some(mut l) = self.link.take() {
            l.close();
        }
        self.welcome = None;
        self.peers.clear();
        self.set_state(LiveState::Disabled)
    }

    fn set_state(&mut self, s: LiveState) -> Vec<LiveEvent> {
        if self.state == s {
            return Vec::new();
        }
        self.state = s;
        vec![LiveEvent::State(s)]
    }

    /// Forgets the connection and schedules the next dial with full jitter.
    fn drop_link(&mut self, now: Instant, backoff: bool) -> Vec<LiveEvent> {
        if let Some(mut l) = self.link.take() {
            l.close();
        }
        self.welcome = None;
        let mut out = Vec::new();
        if !self.peers.is_empty() {
            self.peers.clear();
            out.push(LiveEvent::Peers(Vec::new()));
        }
        if self.state == LiveState::Disabled {
            return out;
        }
        let delay = if backoff {
            self.attempt = self.attempt.saturating_add(1);
            let cap = BACKOFF_CAP_MS.min(BACKOFF_BASE_MS << self.attempt.min(6));
            let r = u64::from_le_bytes(crypto::random_bytes::<8>().unwrap_or([3; 8]));
            r % (cap + 1)
        } else {
            0
        };
        self.next_dial = now + Duration::from_millis(delay);
        out.extend(self.set_state(LiveState::Offline));
        out
    }

    /// One step: dials when due, waits up to `wait` for a frame, keeps the heartbeat, expires
    /// peers. Returns what happened.
    pub fn step(&mut self, wait: Duration) -> Vec<LiveEvent> {
        let now = Instant::now();
        let mut out = Vec::new();
        if self.state == LiveState::Disabled {
            std::thread::sleep(wait.min(Duration::from_millis(50)));
            return out;
        }
        if self.link.is_none() {
            if now < self.next_dial {
                std::thread::sleep(wait.min(self.next_dial - now));
                return out;
            }
            out.extend(self.set_state(LiveState::Connecting));
            let hello = Hello {
                protocol: ProtocolVersion::CURRENT,
                device_id: self.device.clone(),
                last_server_seq: self.last_server_seq,
                presence: self.presence,
            };
            match self.dialer.dial(&hello) {
                Ok((w, link)) => {
                    if w.validate().is_err() {
                        out.extend(self.drop_link(now, true));
                        return out;
                    }
                    // the backoff resets only once the link has lasted (see below), so a server
                    // that answers every welcome with a bye isn't redialled every second
                    self.connected_at = Some(now);
                    self.last_rx = now;
                    self.last_ping = now;
                    self.last_server_seq = self.last_server_seq.max(w.server_seq);
                    let seq = w.server_seq;
                    self.welcome = Some(w);
                    self.link = Some(link);
                    out.extend(self.set_state(LiveState::Live));
                    // always catch up over HTTP after connecting: hints sent while away are gone
                    out.push(LiveEvent::CatchUp(seq));
                }
                Err(DialError::Refused(b)) if b.code == ErrorCode::AccessRevoked => {
                    out.extend(self.stop());
                    out.push(LiveEvent::AccessRemoved);
                }
                Err(DialError::Refused(b)) if b.code == ErrorCode::UpgradeRequired => {
                    out.extend(self.stop());
                }
                Err(DialError::Bye(b)) if !b.code.may_reconnect() => {
                    out.extend(self.stop());
                    if b.code == ByeCode::AccessRevoked {
                        out.push(LiveEvent::AccessRemoved);
                    }
                }
                Err(DialError::Bye(b)) => {
                    out.extend(self.drop_link(now, true));
                    if let Some(ms) = b.retry_after_ms {
                        self.next_dial = self.next_dial.max(now + Duration::from_millis(ms));
                    }
                }
                Err(_) => out.extend(self.drop_link(now, true)),
            }
            return out;
        }
        let heartbeat = Duration::from_millis(
            self.welcome
                .as_ref()
                .map_or(20_000, |w| u64::from(w.heartbeat_ms)),
        );
        let got = self.link.as_mut().expect("connected").recv(wait);
        let now = Instant::now();
        match got {
            Ok(Some(frame)) => {
                self.last_rx = now;
                out.extend(self.handle(frame, now));
            }
            Ok(None) => {}
            Err(_) => {
                out.extend(self.drop_link(now, true));
                return out;
            }
        }
        if self.link.is_none() {
            return out;
        }
        if now.duration_since(self.last_rx) > heartbeat * 2 {
            // silent socket: assume it's gone
            out.extend(self.drop_link(now, true));
            return out;
        }
        if self.attempt > 0
            && self
                .connected_at
                .is_some_and(|t| now.duration_since(t) >= heartbeat * 2)
        {
            self.attempt = 0;
        }
        if now.duration_since(self.last_ping) >= heartbeat {
            self.last_ping = now;
            if self
                .link
                .as_mut()
                .expect("connected")
                .send(&ClientFrame::Ping)
                .is_err()
            {
                out.extend(self.drop_link(now, true));
                return out;
            }
        }
        let before = self.peers.len();
        self.peers.retain(|_, p| p.expires > now);
        if self.peers.len() != before {
            out.push(LiveEvent::Peers(self.peers.values().cloned().collect()));
        }
        out
    }

    fn handle(&mut self, frame: ServerFrame, now: Instant) -> Vec<LiveEvent> {
        match frame {
            ServerFrame::Committed(c) => {
                if c.server_seq <= self.last_server_seq {
                    // a duplicate or late hint: nothing new
                    return Vec::new();
                }
                self.last_server_seq = c.server_seq;
                vec![LiveEvent::CatchUp(c.server_seq)]
            }
            ServerFrame::Awareness(a) => self.receive_awareness(a, now),
            ServerFrame::Membership(m) => {
                let mut out = vec![LiveEvent::Membership {
                    account: m.account_id.clone(),
                    role: m.role,
                    state: m.state,
                }];
                // about this session's own member: access removed
                if m.state == MembershipState::Revoked && m.account_id == self.member {
                    out.extend(self.stop());
                    out.push(LiveEvent::AccessRemoved);
                }
                out
            }
            ServerFrame::Bye(b) => {
                if !b.code.may_reconnect() {
                    let mut out = self.stop();
                    if b.code == ByeCode::AccessRevoked {
                        out.push(LiveEvent::AccessRemoved);
                    }
                    return out;
                }
                let out = self.drop_link(now, true);
                if let Some(ms) = b.retry_after_ms {
                    self.next_dial = self.next_dial.max(now + Duration::from_millis(ms));
                }
                out
            }
            ServerFrame::Ping => {
                if let Some(l) = self.link.as_mut() {
                    let _ = l.send(&ClientFrame::Pong);
                }
                Vec::new()
            }
            ServerFrame::CommentCommitted(c) => vec![LiveEvent::CommentsCatchUp(c.server_seq)],
            ServerFrame::Notification(_) => vec![LiveEvent::Notification],
            // the welcome only comes once, while dialing
            _ => Vec::new(),
        }
    }

    fn receive_awareness(&mut self, a: Awareness, now: Instant) -> Vec<LiveEvent> {
        let (Some(session), Some(member), Some(sender)) =
            (a.session_id.clone(), a.member.clone(), a.device_id.clone())
        else {
            return Vec::new();
        };
        if a.validate().is_err() || Some(session.as_str()) == self.session_id() {
            return Vec::new();
        }
        let Ok(sealed) = base64url::decode(&a.payload) else {
            return Vec::new();
        };
        // a frame that doesn't open (wrong space, forged sender, tampered) is dropped quietly
        let Ok(plain) = crypto::open(&self.key, &awareness_aad(&self.space, &sender), &sealed)
        else {
            return Vec::new();
        };
        let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&plain) else {
            return Vec::new();
        };
        if self.peers.len() >= MAX_PEERS && !self.peers.contains_key(session.as_str()) {
            return Vec::new();
        }
        let ttl = Duration::from_millis(u64::from(a.ttl_ms));
        self.peers.insert(
            session.to_string(),
            Peer {
                session_id: session.to_string(),
                member,
                payload,
                expires: now + ttl,
            },
        );
        vec![LiveEvent::Peers(self.peers.values().cloned().collect())]
    }

    /// Sends this person's awareness (JSON, at most [`MAX_AWARENESS_PLAINTEXT_BYTES`]) sealed for
    /// the space's members. Returns `Ok(false)` when not sent: presence off, not connected, or
    /// over the rate (the next frame supersedes it anyway).
    pub fn send_awareness(&mut self, payload: &serde_json::Value) -> Result<bool, String> {
        let plain = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        if plain.len() > MAX_AWARENESS_PLAINTEXT_BYTES {
            return Err("awareness is too large".into());
        }
        if !self.presence || self.state != LiveState::Live {
            return Ok(false);
        }
        let now = Instant::now();
        self.tokens = (self.tokens
            + now.duration_since(self.tokens_at).as_secs_f64() * AWARENESS_SEND_PER_S)
            .min(AWARENESS_SEND_PER_S);
        self.tokens_at = now;
        if self.tokens < 1.0 {
            return Ok(false);
        }
        self.tokens -= 1.0;
        let nonce_ctx = crypto::random_bytes::<16>().map_err(|e| e.to_string())?;
        let sealed = crypto::seal(
            &self.key,
            &nonce_ctx,
            &awareness_aad(&self.space, &self.device),
            &plain,
        );
        let frame = ClientFrame::Awareness(Awareness {
            session_id: None,
            member: None,
            device_id: None,
            payload: base64url::encode(&sealed),
            ttl_ms: AWARENESS_TTL_MS,
        });
        let link = self.link.as_mut().ok_or("not connected")?;
        link.send(&frame)?;
        Ok(true)
    }

    /// The peers whose awareness hasn't expired.
    pub fn peers(&self) -> Vec<Peer> {
        self.peers.values().cloned().collect()
    }
}

// ----------------------------------------------------------- in-process --

#[cfg(feature = "testkit")]
pub use in_process::InProcessDialer;

#[cfg(feature = "testkit")]
mod in_process {
    use super::*;
    use penroam_sync_testkit::{Caller, InMemoryServer, LiveRefused};
    use std::sync::Arc;

    /// Dials the test server's live hub directly as a member's device (tests).
    pub struct InProcessDialer {
        /// The server.
        pub server: Arc<InMemoryServer>,
        /// The space.
        pub space: AccountId,
        /// The member.
        pub member: AccountId,
        /// The member's device in the space.
        pub device: DeviceId,
        /// Test knob: dialing fails (offline).
        pub offline: Arc<std::sync::atomic::AtomicBool>,
    }

    struct Link {
        server: Arc<InMemoryServer>,
        session: penroam_sync_protocol::LiveSessionId,
        inbox: std::collections::VecDeque<ServerFrame>,
        offline: Arc<std::sync::atomic::AtomicBool>,
    }

    impl LiveLink for Link {
        fn send(&mut self, frame: &ClientFrame) -> Result<(), String> {
            if !self.server.live_is_open(&self.session)
                || self.offline.load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err("closed".into());
            }
            self.server.live_send(&self.session, frame);
            Ok(())
        }
        fn recv(&mut self, wait: Duration) -> Result<Option<ServerFrame>, String> {
            if self.offline.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("offline".into());
            }
            if self.inbox.is_empty() {
                self.inbox.extend(self.server.live_recv(&self.session));
            }
            if let Some(f) = self.inbox.pop_front() {
                return Ok(Some(f));
            }
            if !self.server.live_is_open(&self.session) {
                return Err("closed".into());
            }
            std::thread::sleep(wait.min(Duration::from_millis(5)));
            Ok(None)
        }
        fn close(&mut self) {
            self.server.live_disconnect(&self.session);
        }
    }

    impl LiveDialer for InProcessDialer {
        fn dial(&mut self, hello: &Hello) -> Result<(Welcome, Box<dyn LiveLink>), DialError> {
            if self.offline.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(DialError::Network("offline".into()));
            }
            match self.server.live_connect(
                Caller::member(&self.space, &self.member),
                &self.device,
                hello,
            ) {
                Ok(w) => {
                    let link = Link {
                        server: self.server.clone(),
                        session: w.session_id.clone(),
                        inbox: Default::default(),
                        offline: self.offline.clone(),
                    };
                    Ok((w, Box::new(link)))
                }
                Err(LiveRefused::Http(b)) => Err(DialError::Refused(b)),
                Err(LiveRefused::Bye(b)) => Err(DialError::Bye(b)),
            }
        }
    }
}

// ------------------------------------------------------------ WebSocket --

#[cfg(feature = "http")]
pub use ws::WsDialer;

#[cfg(feature = "http")]
mod ws {
    use super::*;
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};
    use tungstenite::client::IntoClientRequest;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Connector, Message, WebSocket};

    /// Dials `GET /v1/live` with the space's device session (`Authorization: Bearer`, never in
    /// the URL), over TLS for `https://` servers (rustls, web PKI roots).
    pub struct WsDialer {
        /// Server base URL (`https://…` or, for the development server, `http://…`).
        pub base_url: String,
        /// The space's current device session token (the app refreshes it here).
        pub token: Arc<Mutex<String>>,
    }

    struct Link {
        ws: WebSocket<MaybeTlsStream<TcpStream>>,
        max: u32,
    }

    fn tcp(ws: &WebSocket<MaybeTlsStream<TcpStream>>) -> Option<&TcpStream> {
        match ws.get_ref() {
            MaybeTlsStream::Plain(s) => Some(s),
            MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
            _ => None,
        }
    }

    impl LiveLink for Link {
        fn send(&mut self, frame: &ClientFrame) -> Result<(), String> {
            self.ws
                .send(Message::text(frame.to_text()))
                .map_err(|e| e.to_string())
        }
        fn recv(&mut self, wait: Duration) -> Result<Option<ServerFrame>, String> {
            if let Some(s) = tcp(&self.ws) {
                let _ = s.set_read_timeout(Some(wait.max(Duration::from_millis(1))));
            }
            match self.ws.read() {
                Ok(Message::Text(t)) => ServerFrame::parse(t.as_str(), self.max)
                    .map(Some)
                    .map_err(|e| e.to_string()),
                Ok(Message::Ping(p)) => {
                    let _ = self.ws.send(Message::Pong(p));
                    Ok(None)
                }
                Ok(Message::Close(_)) => Err("closed".into()),
                Ok(_) => Ok(None),
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(None)
                }
                Err(e) => Err(e.to_string()),
            }
        }
        fn close(&mut self) {
            let _ = self.ws.close(None);
            let _ = self.ws.flush();
        }
    }

    fn tls() -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
        )
    }

    impl LiveDialer for WsDialer {
        fn dial(&mut self, hello: &Hello) -> Result<(Welcome, Box<dyn LiveLink>), DialError> {
            let base = self.base_url.trim_end_matches('/');
            let url = if let Some(rest) = base.strip_prefix("https://") {
                format!("wss://{rest}{}", penroam_sync_protocol::routes::LIVE)
            } else if let Some(rest) = base.strip_prefix("http://") {
                format!("ws://{rest}{}", penroam_sync_protocol::routes::LIVE)
            } else {
                return Err(DialError::Network("unsupported server URL".into()));
            };
            let mut req = url
                .as_str()
                .into_client_request()
                .map_err(|e| DialError::Network(e.to_string()))?;
            let token = self.token.lock().map(|t| t.clone()).unwrap_or_default();
            req.headers_mut().insert(
                "Authorization",
                format!("Bearer {token}")
                    .parse()
                    .map_err(|_| DialError::Network("bad token".into()))?,
            );
            let host = req.uri().host().unwrap_or_default().to_string();
            let port =
                req.uri()
                    .port_u16()
                    .unwrap_or(if url.starts_with("wss") { 443 } else { 80 });
            let stream = TcpStream::connect((host.as_str(), port))
                .map_err(|e| DialError::Network(e.to_string()))?;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
            let connector = url.starts_with("wss").then(|| Connector::Rustls(tls()));
            let (mut ws, _) = tungstenite::client_tls_with_config(req, stream, None, connector)
                .map_err(|e| match e {
                    tungstenite::HandshakeError::Failure(tungstenite::Error::Http(resp)) => {
                        let body = resp
                            .body()
                            .as_ref()
                            .and_then(|b| serde_json::from_slice::<ErrorBody>(b).ok());
                        match body {
                            Some(b) => DialError::Refused(b),
                            None => DialError::Network(format!("HTTP {}", resp.status())),
                        }
                    }
                    other => DialError::Network(other.to_string()),
                })?;
            ws.send(Message::text(ClientFrame::Hello(hello.clone()).to_text()))
                .map_err(|e| DialError::Network(e.to_string()))?;
            let max = penroam_sync_protocol::live::MAX_MAX_FRAME_BYTES;
            loop {
                match ws.read() {
                    Ok(Message::Text(t)) => {
                        return match ServerFrame::parse(t.as_str(), max) {
                            Ok(ServerFrame::Welcome(w)) => {
                                let max = w.max_frame_bytes;
                                Ok((w, Box::new(Link { ws, max })))
                            }
                            Ok(ServerFrame::Bye(b)) => Err(DialError::Bye(b)),
                            Ok(_) => Err(DialError::Network("expected welcome".into())),
                            Err(e) => Err(DialError::Network(e.to_string())),
                        }
                    }
                    Ok(Message::Close(_)) => return Err(DialError::Network("closed".into())),
                    Ok(_) => continue,
                    Err(e) => return Err(DialError::Network(e.to_string())),
                }
            }
        }
    }
}
