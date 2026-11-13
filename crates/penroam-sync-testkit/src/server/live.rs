//! The live channel hub of the development test server (`live.v1`, protocol 1.8).
//!
//! Sessions live in [`State`], so they share the server's single lock with every durable write:
//! a `committed` hint is queued only after the push that grew the log has returned, and a
//! membership change closes the member's sessions in the same critical section that changed it.
//! The hub never writes notebook data.
//!
//! In-process callers drive sessions by id ([`InMemoryServer::live_connect`],
//! [`InMemoryServer::live_send_text`], [`InMemoryServer::live_recv`]); the HTTP test server's
//! WebSocket loop uses the same calls. Each session has a bounded queue of outgoing frames:
//!
//! * `committed` hints coalesce into one queued hint, so a stalled reader holds at most one; a hint
//!   that can't be queued at all ends the session with `slow_consumer`;
//! * awareness to a full queue is dropped, and too many drops end the session with
//!   `slow_consumer`;
//! * awareness from a session is rate-limited by a per-session and a per-space token bucket on
//!   the test clock; sustained excess ends the session with `rate_limited`.
//!
//! Membership is re-checked on every client frame and on [`InMemoryServer::live_tick`], and
//! immediately when it changes. See `docs/protocol/sync-protocol-v1.md` §12g.

use std::collections::{BTreeMap, VecDeque};

use penroam_sync_protocol::live::{
    Awareness, Bye, ByeCode, ClientFrame, CommentCommitted, Committed, Hello, Membership,
    NotificationHint, ServerFrame, Welcome, AWARENESS_BURST, AWARENESS_RATE_PER_S,
    COMMENTS_MIN_PROTOCOL, DEFAULT_HEARTBEAT_MS, DEFAULT_MAX_FRAME_BYTES,
};
use penroam_sync_protocol::sharing::{MembershipState, Role};
use penroam_sync_protocol::{AccountId, DeviceId, ErrorBody, ErrorCode, LiveSessionId};

use super::sharing::now_ms;
use super::{Caller, InMemoryServer, State};

/// Tunables of the live hub. [`Default`] gives the documented defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveConfig {
    /// Advertised heartbeat interval. Sessions silent for twice this are closed (`idle_timeout`),
    /// and relayed awareness TTLs are capped at twice this.
    pub heartbeat_ms: u32,
    /// Advertised frame limit; larger client frames end the session (`invalid_frame`).
    pub max_frame_bytes: u32,
    /// Outgoing frames queued per session before the slow-consumer rules apply.
    pub queue_frames: usize,
    /// Live sessions one device may hold in one space; one more is refused (`rate_limited`).
    pub sessions_per_device: usize,
    /// Awareness frames per second per session, and the burst on top.
    pub session_rate_per_s: u32,
    /// Burst of the per-session bucket.
    pub session_burst: u32,
    /// Awareness frames per second per space (all sessions together), and its burst.
    pub space_rate_per_s: u32,
    /// Burst of the per-space bucket.
    pub space_burst: u32,
    /// Consecutive rate-limited awareness frames from one session before it is closed.
    pub rate_strikes: u32,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            heartbeat_ms: DEFAULT_HEARTBEAT_MS,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            queue_frames: 64,
            sessions_per_device: 4,
            session_rate_per_s: AWARENESS_RATE_PER_S,
            session_burst: AWARENESS_BURST,
            space_rate_per_s: 4 * AWARENESS_RATE_PER_S,
            space_burst: 4 * AWARENESS_BURST,
            rate_strikes: AWARENESS_BURST,
        }
    }
}

/// Why [`InMemoryServer::live_connect`] refused a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveRefused {
    /// Refused before the upgrade, as an HTTP error (unknown account, not a member, removed
    /// member, not a space, no device session, too many sessions).
    Http(ErrorBody),
    /// The hello was refused after the upgrade; the server sends this `bye` and closes.
    Bye(Bye),
}

/// One live presence entry: a session whose last awareness hasn't expired yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePresence {
    /// The session.
    pub session_id: LiveSessionId,
    /// Its verified member.
    pub member: AccountId,
    /// Its device.
    pub device: DeviceId,
    /// When its awareness expires (test clock, Unix ms).
    pub expires_at_ms: u64,
}

/// A token bucket in thousandths of a token, refilled from the test clock.
#[derive(Debug, Clone)]
struct Bucket {
    milli: u64,
    last_ms: u64,
}

impl Bucket {
    fn new(burst: u32, now: u64) -> Self {
        Self {
            milli: u64::from(burst) * 1000,
            last_ms: now,
        }
    }

    fn take(&mut self, rate_per_s: u32, burst: u32, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now);
        // rate tokens per 1000 ms = rate milli-tokens per ms
        self.milli = (self.milli + elapsed * u64::from(rate_per_s)).min(u64::from(burst) * 1000);
        if self.milli >= 1000 {
            self.milli -= 1000;
            true
        } else {
            false
        }
    }
}

struct Session {
    space: AccountId,
    member: AccountId,
    device: DeviceId,
    role: Role,
    presence: bool,
    /// Whether the hello said protocol 1.9 or later: only then does the session get comment
    /// frames and see the `commenter` role (older clients would fail to parse them).
    comments: bool,
    queue: VecDeque<ServerFrame>,
    /// Set once the session is ending; its queue then holds only what's left to say.
    closed: bool,
    bucket: Bucket,
    strikes: u32,
    /// Awareness dropped because the queue was full, since the reader last drained it.
    overflow: usize,
    last_seen_ms: u64,
    presence_until_ms: Option<u64>,
}

/// Hint fault knobs.
#[derive(Default)]
struct HintFaults {
    drop_next: u32,
    duplicate_next: u32,
    hold: bool,
    held: Vec<AccountId>,
}

/// The hub's part of the server state (never persisted: live sessions don't survive a restart).
#[derive(Default)]
pub(super) struct LiveState {
    sessions: BTreeMap<LiveSessionId, Session>,
    last_hinted: BTreeMap<AccountId, u64>,
    space_buckets: BTreeMap<AccountId, Bucket>,
    faults: HintFaults,
    pub(super) config: LiveConfig,
    pub(super) allowed_origins: Vec<String>,
}

impl LiveState {
    /// Ends a session: drops whatever was queued, then queues `before` (if any) and the `bye`.
    fn close(&mut self, id: &LiveSessionId, before: Option<ServerFrame>, bye: Bye) {
        if let Some(s) = self.sessions.get_mut(id) {
            if s.closed {
                return;
            }
            s.queue.clear();
            s.queue.extend(before);
            s.queue.push_back(ServerFrame::Bye(bye));
            s.closed = true;
            s.presence_until_ms = None;
        }
    }

    fn bye(code: ByeCode) -> Bye {
        Bye {
            code,
            retry_after_ms: None,
        }
    }

    /// Queues a frame for one session under the slow-consumer rules.
    fn enqueue(&mut self, id: &LiveSessionId, frame: ServerFrame, coalesce: bool) {
        let cap = self.config.queue_frames;
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        if s.closed {
            return;
        }
        let mut frame = frame;
        match &mut frame {
            ServerFrame::CommentCommitted(_) | ServerFrame::Notification(_) if !s.comments => {
                return;
            }
            ServerFrame::Membership(m) => m.role = visible_role(m.role, s.comments),
            _ => {}
        }
        let overflow = match &frame {
            ServerFrame::CommentCommitted(c) if coalesce => {
                if let Some(ServerFrame::CommentCommitted(q)) = s
                    .queue
                    .iter_mut()
                    .find(|f| matches!(f, ServerFrame::CommentCommitted(_)))
                {
                    q.server_seq = q.server_seq.max(c.server_seq);
                    return;
                }
                s.queue.len() >= cap
            }
            ServerFrame::Committed(c) if coalesce => {
                // One queued hint is enough: raise it instead of adding another.
                if let Some(ServerFrame::Committed(q)) = s
                    .queue
                    .iter_mut()
                    .find(|f| matches!(f, ServerFrame::Committed(_)))
                {
                    q.server_seq = q.server_seq.max(c.server_seq);
                    return;
                }
                s.queue.len() >= cap
            }
            ServerFrame::Awareness(_) if s.queue.len() >= cap => {
                s.overflow += 1;
                if s.overflow < cap {
                    return;
                }
                true
            }
            _ => s.queue.len() >= cap,
        };
        if overflow {
            self.close(id, None, Self::bye(ByeCode::SlowConsumer));
        } else {
            s.queue.push_back(frame);
        }
    }

    fn sessions_of(&self, space: &AccountId) -> Vec<LiveSessionId> {
        self.sessions
            .iter()
            .filter(|(_, s)| &s.space == space && !s.closed)
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn hint(&mut self, space: &AccountId, server_seq: u64, duplicate: bool) {
        for id in self.sessions_of(space) {
            self.enqueue(&id, ServerFrame::Committed(Committed { server_seq }), true);
            if duplicate {
                self.enqueue(&id, ServerFrame::Committed(Committed { server_seq }), false);
            }
        }
    }
}

/// The role a session is shown: `commenter` reads as `viewer` (also read-only for documents) to a
/// session from before protocol 1.9, which can't parse it.
fn visible_role(role: Role, knows_commenter: bool) -> Role {
    if role == Role::Commenter && !knows_commenter {
        Role::Viewer
    } else {
        role
    }
}

/// After comments were committed to `space`'s comment stream: a coalesced `comment_committed`
/// hint to the space's sessions (1.9 and later).
pub(super) fn comments_committed(st: &mut State, space: &AccountId, server_seq: u64) {
    for id in st.live.sessions_of(space) {
        st.live.enqueue(
            &id,
            ServerFrame::CommentCommitted(CommentCommitted { server_seq }),
            true,
        );
    }
}

/// A new notification for `recipient`: a `notification` hint on every live session of theirs,
/// whatever space it is for (1.9 and later).
pub(super) fn notification(
    st: &mut State,
    recipient: &AccountId,
    id: &penroam_sync_protocol::NotificationId,
    space: &AccountId,
) {
    let ids: Vec<LiveSessionId> = st
        .live
        .sessions
        .iter()
        .filter(|(_, s)| &s.member == recipient && !s.closed)
        .map(|(id, _)| id.clone())
        .collect();
    for sid in ids {
        st.live.enqueue(
            &sid,
            ServerFrame::Notification(NotificationHint {
                notification_id: id.clone(),
                space_id: space.clone(),
            }),
            false,
        );
    }
}

/// A membership change in `space`: every session of the space hears it, and a revoked member's
/// sessions end with `access_revoked` right after the frame naming them.
pub(super) fn member_changed(
    st: &mut State,
    space: &AccountId,
    member: &AccountId,
    role: Role,
    state: MembershipState,
) {
    let frame = ServerFrame::Membership(Membership {
        account_id: member.clone(),
        role,
        state,
    });
    for id in st.live.sessions_of(space) {
        let theirs = st
            .live
            .sessions
            .get(&id)
            .is_some_and(|s| &s.member == member);
        if theirs && state == MembershipState::Revoked {
            st.live.close(
                &id,
                Some(frame.clone()),
                LiveState::bye(ByeCode::AccessRevoked),
            );
        } else {
            if theirs {
                if let Some(s) = st.live.sessions.get_mut(&id) {
                    s.role = role;
                }
            }
            st.live.enqueue(&id, frame.clone(), false);
        }
    }
}

/// Account erasure: every live session of `member`, or in one of the `gone` spaces, ends with
/// `access_revoked`.
pub(super) fn erase(st: &mut State, member: &AccountId, gone: &[AccountId]) {
    let ids: Vec<LiveSessionId> = st
        .live
        .sessions
        .iter()
        .filter(|(_, s)| &s.member == member || gone.contains(&s.space))
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        st.live
            .close(&id, None, LiveState::bye(ByeCode::AccessRevoked));
    }
}

/// Re-checks one session against the space's current membership and its device; closes it with
/// `access_revoked` when either is gone. Returns whether it is still open.
fn recheck(st: &mut State, id: &LiveSessionId) -> bool {
    let Some(s) = st.live.sessions.get(id) else {
        return false;
    };
    if s.closed {
        return false;
    }
    let acct = st.accounts.get(&s.space);
    let ok = acct.is_some_and(|a| {
        a.devices.get(&s.device).is_some_and(|d| !d.revoked)
            && a.space
                .as_ref()
                .is_some_and(|sp| sp.resolve(a, Some(&s.member), Some(&s.device)).is_ok())
    });
    if !ok {
        st.live
            .close(id, None, LiveState::bye(ByeCode::AccessRevoked));
    }
    ok
}

impl InMemoryServer {
    // ---- configuration and fault knobs ----

    /// Replaces the live hub's tunables (affects new sessions' advertised limits immediately).
    pub fn set_live_config(&self, config: LiveConfig) {
        self.lock().live.config = config;
    }

    /// The live hub's tunables.
    pub fn live_config(&self) -> LiveConfig {
        self.lock().live.config.clone()
    }

    /// Browser `Origin` values the HTTP test server accepts on `/v1/live`. Requests without an
    /// `Origin` header (native clients) are always allowed; with one, it must be listed. Empty by
    /// default.
    pub fn set_live_allowed_origins(&self, origins: Vec<String>) {
        self.lock().live.allowed_origins = origins;
    }

    /// Whether `origin` may open `/v1/live`.
    pub(crate) fn live_origin_allowed(&self, origin: &str) -> bool {
        self.lock().live.allowed_origins.iter().any(|o| o == origin)
    }

    /// The next `n` commit hints are not sent (the durable log is unaffected).
    pub fn live_drop_next_hints(&self, n: u32) {
        self.lock().live.faults.drop_next += n;
    }

    /// The next `n` commit hints are sent twice, uncoalesced.
    pub fn live_duplicate_next_hints(&self, n: u32) {
        self.lock().live.faults.duplicate_next += n;
    }

    /// While `true`, commit hints are held back; setting it to `false` sends one hint per space
    /// that committed meanwhile (with its latest `server_seq`).
    pub fn live_hold_hints(&self, hold: bool) {
        let mut st = self.lock();
        st.live.faults.hold = hold;
        if !hold {
            let held = std::mem::take(&mut st.live.faults.held);
            for space in held {
                let seq = st.accounts.get(&space).map_or(0, |a| a.log.len() as u64);
                st.live.hint(&space, seq, false);
            }
        }
    }

    // ---- sessions ----

    /// The pre-upgrade check of `GET /v1/live`: `caller` must be a member's device session on a
    /// sharing space, with a registered, unrevoked device. Errors are what the HTTP server answers
    /// instead of upgrading: `unauthenticated` (unknown account), `forbidden` (not a space, or a
    /// device registered by someone else), `not_found` (not a member), `access_revoked` (removed),
    /// `device_revoked`.
    pub fn live_authorize<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
    ) -> Result<(Role, AccountId), ErrorBody> {
        let caller = caller.into();
        let st = self.lock();
        Self::live_check(&st, caller, device)
    }

    fn live_check(
        st: &State,
        caller: Caller<'_>,
        device: &DeviceId,
    ) -> Result<(Role, AccountId), ErrorBody> {
        let e = |code, m: &str| ErrorBody::new(code, m, "live");
        let acct = st
            .accounts
            .get(caller.account)
            .ok_or_else(|| e(ErrorCode::Unauthenticated, "unknown account"))?;
        let space = acct.space.as_ref().ok_or_else(|| {
            e(
                ErrorCode::Forbidden,
                "the live channel is only for shared notebooks",
            )
        })?;
        let (role, member) = space
            .resolve(acct, caller.member, Some(device))
            .map_err(|(code, m)| e(code, m))?;
        match acct.devices.get(device) {
            None => Err(e(ErrorCode::Forbidden, "device not registered")),
            Some(d) if d.revoked => Err(e(ErrorCode::DeviceRevoked, "device revoked")),
            Some(_) => Ok((role, member)),
        }
    }

    /// Opens a live session for a member's device on a space, as `GET /v1/live` plus `hello` do.
    /// The hello's protocol must be at least 1.8 (`bye upgrade_required` otherwise) and its
    /// `device_id` must be the session's device (`bye unauthenticated`).
    pub fn live_connect<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        hello: &Hello,
    ) -> Result<Welcome, LiveRefused> {
        let caller = caller.into();
        let mut guard = self.lock();
        let st: &mut State = &mut guard;
        let (role, member) = Self::live_check(st, caller, device).map_err(LiveRefused::Http)?;
        hello
            .check_protocol()
            .map_err(|code| LiveRefused::Bye(LiveState::bye(code)))?;
        if &hello.device_id != device {
            return Err(LiveRefused::Bye(LiveState::bye(ByeCode::Unauthenticated)));
        }
        let space = caller.account.clone();
        let cfg = st.live.config.clone();
        let open = st
            .live
            .sessions
            .values()
            .filter(|s| !s.closed && s.space == space && &s.device == device)
            .count();
        if open >= cfg.sessions_per_device {
            let mut b = ErrorBody::new(
                ErrorCode::RateLimited,
                "too many live sessions for this device",
                "live",
            );
            b.retry_after_ms = Some(5_000);
            return Err(LiveRefused::Http(b));
        }
        let now = now_ms(st);
        let id: LiveSessionId = format!("ls-{}", uuid::Uuid::new_v4().simple())
            .parse()
            .expect("hex digits with a prefix are a valid id");
        let server_seq = st.accounts[&space].log.len() as u64;
        let hinted = st.live.last_hinted.entry(space.clone()).or_insert(0);
        *hinted = (*hinted).max(server_seq);
        st.live.sessions.insert(
            id.clone(),
            Session {
                space,
                member,
                device: device.clone(),
                role,
                presence: hello.presence,
                comments: hello.protocol >= COMMENTS_MIN_PROTOCOL,
                queue: VecDeque::new(),
                closed: false,
                bucket: Bucket::new(cfg.session_burst, now),
                strikes: 0,
                overflow: 0,
                last_seen_ms: now,
                presence_until_ms: None,
            },
        );
        Ok(Welcome {
            session_id: id,
            server_seq,
            heartbeat_ms: cfg.heartbeat_ms,
            max_frame_bytes: cfg.max_frame_bytes,
            role: visible_role(role, hello.protocol >= COMMENTS_MIN_PROTOCOL),
        })
    }

    /// Handles one text frame from a session's client. Frames that are too large, malformed, a
    /// second `hello` or out of range end the session with `invalid_frame`. Every frame re-checks
    /// membership first.
    pub fn live_send_text(&self, session: &LiveSessionId, text: &str) {
        let mut guard = self.lock();
        let st: &mut State = &mut guard;
        if !recheck(st, session) {
            return;
        }
        let now = now_ms(st);
        let cfg = st.live.config.clone();
        let frame = match ClientFrame::parse(text, cfg.max_frame_bytes) {
            Ok(f) => f,
            Err(e) => {
                st.live.close(session, None, LiveState::bye(e.bye_code()));
                return;
            }
        };
        let s = st.live.sessions.get_mut(session).expect("rechecked");
        s.last_seen_ms = now;
        match frame {
            ClientFrame::Hello(_) => {
                st.live
                    .close(session, None, LiveState::bye(ByeCode::InvalidFrame));
            }
            ClientFrame::Ping => st.live.enqueue(session, ServerFrame::Pong, false),
            ClientFrame::Pong => {}
            ClientFrame::Awareness(a) => self.relay(st, session, a, now),
        }
    }

    /// Handles a typed client frame (serialized, then [`Self::live_send_text`]).
    pub fn live_send(&self, session: &LiveSessionId, frame: &ClientFrame) {
        self.live_send_text(session, &frame.to_text());
    }

    fn relay(&self, st: &mut State, session: &LiveSessionId, mut a: Awareness, now: u64) {
        let cfg = st.live.config.clone();
        let s = st.live.sessions.get_mut(session).expect("rechecked");
        if !s.presence {
            return; // presence off: the client said it wouldn't send any
        }
        if !s
            .bucket
            .take(cfg.session_rate_per_s, cfg.session_burst, now)
        {
            s.strikes += 1;
            if s.strikes >= cfg.rate_strikes {
                st.live.close(
                    session,
                    None,
                    Bye {
                        code: ByeCode::RateLimited,
                        retry_after_ms: Some(1_000),
                    },
                );
            }
            return;
        }
        s.strikes = 0;
        let space = s.space.clone();
        let space_ok = st
            .live
            .space_buckets
            .entry(space.clone())
            .or_insert_with(|| Bucket::new(cfg.space_burst, now))
            .take(cfg.space_rate_per_s, cfg.space_burst, now);
        if !space_ok {
            return;
        }
        let s = st.live.sessions.get_mut(session).expect("rechecked");
        // Whatever the sender claimed, the relayed identity is the verified one.
        a.session_id = Some(session.clone());
        a.member = Some(s.member.clone());
        a.device_id = Some(s.device.clone());
        a.ttl_ms = a.ttl_ms.min(cfg.heartbeat_ms.saturating_mul(2));
        s.presence_until_ms = Some(now + u64::from(a.ttl_ms));
        for other in st.live.sessions_of(&space) {
            if &other != session {
                st.live
                    .enqueue(&other, ServerFrame::Awareness(a.clone()), false);
            }
        }
    }

    /// Takes every frame queued for a session, oldest first. After the `bye` has been taken the
    /// session is gone; an unknown session yields nothing.
    pub fn live_recv(&self, session: &LiveSessionId) -> Vec<ServerFrame> {
        let mut st = self.lock();
        let Some(s) = st.live.sessions.get_mut(session) else {
            return Vec::new();
        };
        let frames: Vec<ServerFrame> = s.queue.drain(..).collect();
        s.overflow = 0;
        if s.closed {
            st.live.sessions.remove(session);
        }
        frames
    }

    /// Whether a session is open (not closed by either side).
    pub fn live_is_open(&self, session: &LiveSessionId) -> bool {
        self.lock()
            .live
            .sessions
            .get(session)
            .is_some_and(|s| !s.closed)
    }

    /// The client went away: forget the session.
    pub fn live_disconnect(&self, session: &LiveSessionId) {
        self.lock().live.sessions.remove(session);
    }

    /// Housekeeping on the test clock: re-checks every session's membership, closes sessions
    /// silent for more than two heartbeats (`idle_timeout`), and expires presence.
    pub fn live_tick(&self) {
        let mut guard = self.lock();
        let st: &mut State = &mut guard;
        let now = now_ms(st);
        let idle = u64::from(st.live.config.heartbeat_ms) * 2;
        let ids: Vec<LiveSessionId> = st.live.sessions.keys().cloned().collect();
        for id in ids {
            if !recheck(st, &id) {
                continue;
            }
            let s = st.live.sessions.get_mut(&id).expect("rechecked");
            if s.presence_until_ms.is_some_and(|t| t <= now) {
                s.presence_until_ms = None;
            }
            if now.saturating_sub(s.last_seen_ms) > idle {
                st.live
                    .close(&id, None, LiveState::bye(ByeCode::IdleTimeout));
            }
        }
    }

    /// Sessions of `space` whose last awareness hasn't expired, by session id.
    pub fn live_presence(&self, space: &AccountId) -> Vec<LivePresence> {
        let st = self.lock();
        let now = now_ms(&st);
        st.live
            .sessions
            .iter()
            .filter(|(_, s)| &s.space == space && !s.closed)
            .filter_map(|(id, s)| {
                let until = s.presence_until_ms.filter(|&t| t > now)?;
                Some(LivePresence {
                    session_id: id.clone(),
                    member: s.member.clone(),
                    device: s.device.clone(),
                    expires_at_ms: until,
                })
            })
            .collect()
    }

    /// Graceful shutdown: every session gets `bye server_restart` with `retry_after_ms`.
    pub fn live_shutdown(&self, retry_after_ms: u64) {
        let mut st = self.lock();
        let ids: Vec<LiveSessionId> = st.live.sessions.keys().cloned().collect();
        for id in ids {
            st.live.close(
                &id,
                None,
                Bye {
                    code: ByeCode::ServerRestart,
                    retry_after_ms: Some(retry_after_ms),
                },
            );
        }
    }

    /// After a push to `account`: if it is a space whose log grew past the last hint, queue a
    /// `committed` hint for its sessions (subject to the hint fault knobs).
    pub(super) fn live_notify_commit(&self, account: &AccountId) {
        let mut st = self.lock();
        let Some(a) = st.accounts.get(account) else {
            return;
        };
        if a.space.is_none() {
            return;
        }
        let seq = a.log.len() as u64;
        let last = st.live.last_hinted.entry(account.clone()).or_insert(0);
        if seq <= *last {
            return;
        }
        *last = seq;
        let f = &mut st.live.faults;
        if f.hold {
            if !f.held.contains(account) {
                f.held.push(account.clone());
            }
            return;
        }
        if f.drop_next > 0 {
            f.drop_next -= 1;
            return;
        }
        let duplicate = f.duplicate_next > 0;
        if duplicate {
            f.duplicate_next -= 1;
        }
        st.live.hint(account, seq, duplicate);
    }

    /// After a device was revoked: its live sessions in that account end with `access_revoked`.
    pub(super) fn live_device_revoked(&self, account: &AccountId, device: &DeviceId) {
        let mut st = self.lock();
        let ids: Vec<LiveSessionId> = st
            .live
            .sessions
            .iter()
            .filter(|(_, s)| &s.space == account && &s.device == device)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            st.live
                .close(&id, None, LiveState::bye(ByeCode::AccessRevoked));
        }
    }
}
