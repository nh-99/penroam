//! Live collaboration in the app: one live channel per active shared notebook
//! (`penroam_sync::live`), the `live://presence` and `live://status` events, the
//! `live_awareness` and `live_prefs` commands.
//!
//! The sync worker (`crate::sync`) starts a channel for every shared notebook that is active on
//! this device, with the space's device session, and stops it when the notebook leaves. Each
//! channel runs on its own thread around a [`LiveClient`]:
//!
//! * a commit hint, or (re)connecting, asks the worker to sync that space at once
//!   (`Cmd::SpaceHint`); the hint only makes the pull sooner, the pull is what counts;
//! * a live notice that access was removed (a `bye`, or a membership change) is only a cue: the
//!   channel stops, and the worker checks over HTTP (`Cmd::SpaceRevoked` →
//!   `SpaceSet::confirm_access`); the notebook goes read-only only if the server's list or an
//!   authenticated `access_revoked` confirms it, and otherwise the channel starts again;
//! * peers' awareness goes to the editor as `live://presence`, with each person's verified email
//!   and role from the members list (a neutral label until it is known), and a colour index that
//!   stays the same for a person.
//!
//! Awareness from the editor is checked here (kind, sizes, coordinates), sealed and sent at most
//! 15 times a second; over that rate only the newest frame of each kind waits to go out. With "share presence" off ([`LivePrefs`]) nothing is sent,
//! but hints still arrive.

use crate::commands::CmdError;
use crate::sharing::SharedRole;
use crate::AppState;
use penroam_storage::ErrorCode;
use penroam_sync::live::{LiveClient, LiveEvent, LiveState, Peer, WsDialer};
use penroam_sync_protocol::{AccountId, DeviceId, Role};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use ts_rs::TS;
use uuid::Uuid;

/// A rectangle in page coordinates (points).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PageRect {
    /// Left.
    pub x: f64,
    /// Top.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// A caret in page coordinates: the top of its line and the line's height.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PageCaret {
    /// Left.
    pub x: f64,
    /// Top of the line.
    pub y: f64,
    /// Line height.
    pub h: f64,
}

/// What an awareness frame says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum AwarenessKind {
    /// On a page.
    Here,
    /// A caret in a text block.
    Caret,
    /// A selection.
    Selection,
    /// The area being looked at.
    Viewport,
}

/// What this device tells the others (from the editor).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AwarenessPayload {
    /// The page.
    pub page_id: Uuid,
    /// What it says.
    pub kind: AwarenessKind,
    /// For a selection or viewport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rect: Option<PageRect>,
    /// For a caret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub caret: Option<PageCaret>,
    /// The text block a caret or selection is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub block_id: Option<Uuid>,
}

/// The sealed plaintext, as the contract names its fields (snake case).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Wire {
    page_id: Uuid,
    kind: AwarenessKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rect: Option<PageRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    caret: Option<PageCaret>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    block_id: Option<Uuid>,
}

/// Largest coordinate accepted (points); anything bigger is nonsense or hostile.
const MAX_COORD: f64 = 1.0e7;

impl AwarenessPayload {
    fn check(&self) -> Result<(), String> {
        let ok = |v: f64| v.is_finite() && v.abs() <= MAX_COORD;
        if let Some(r) = self.rect {
            if ![r.x, r.y, r.w, r.h].into_iter().all(ok) || r.w < 0.0 || r.h < 0.0 {
                return Err("bad rectangle".into());
            }
        }
        if let Some(c) = self.caret {
            if ![c.x, c.y, c.h].into_iter().all(ok) || c.h < 0.0 {
                return Err("bad caret".into());
            }
        }
        Ok(())
    }

    fn wire(&self) -> Wire {
        Wire {
            page_id: self.page_id,
            kind: self.kind,
            rect: self.rect,
            caret: self.caret,
            block_id: self.block_id,
        }
    }
}

/// One other person's session.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PresencePeer {
    /// Their live session.
    pub session_id: String,
    /// Verified email, empty while unknown (shown as a neutral label).
    pub member_display: String,
    /// Their role.
    pub role: SharedRole,
    /// The page they are on.
    pub page_id: Uuid,
    /// What the entry says.
    pub kind: AwarenessKind,
    /// Selection or viewport.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rect: Option<PageRect>,
    /// Caret.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub caret: Option<PageCaret>,
    /// Text block of the caret or selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub block_id: Option<Uuid>,
    /// Palette index, the same for a person on every device.
    pub color_index: u32,
    /// Unix ms after which the entry is stale.
    #[ts(type = "number")]
    pub expires_at: u64,
}

/// `live://presence`.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PresenceEvent {
    /// The shared notebook.
    pub notebook_id: Uuid,
    /// Everyone else there now.
    pub peers: Vec<PresencePeer>,
}

/// The live channel's state, for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum LiveStateView {
    /// Dialing.
    Connecting,
    /// Connected.
    Live,
    /// Not connected; ordinary sync goes on.
    Offline,
    /// Off (access removed, or an update needed).
    Disabled,
}

impl From<LiveState> for LiveStateView {
    fn from(s: LiveState) -> Self {
        match s {
            LiveState::Connecting => Self::Connecting,
            LiveState::Live => Self::Live,
            LiveState::Offline => Self::Offline,
            LiveState::Disabled => Self::Disabled,
        }
    }
}

/// `live://status`.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LiveStatusEvent {
    /// The shared notebook.
    pub notebook_id: Uuid,
    /// Its live channel's state.
    pub state: LiveStateView,
}

/// The presence preference of this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LivePrefs {
    /// Tell others which page and where this person is. Edits sync either way.
    pub share_presence: bool,
}

impl Default for LivePrefs {
    fn default() -> Self {
        Self {
            share_presence: true,
        }
    }
}

fn prefs_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("live.json"))
}

/// The preference (default when unset or unreadable).
pub fn load_prefs(app: &AppHandle) -> LivePrefs {
    prefs_path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Messages to one channel's thread.
enum Ctl {
    Awareness(AwarenessPayload),
    Presence(bool),
    Stop,
}

/// One running channel.
struct Handle {
    tx: Sender<Ctl>,
    notebook: Uuid,
    token: Arc<Mutex<String>>,
    state: Arc<Mutex<LiveState>>,
    members: Arc<Mutex<HashMap<String, (String, Role)>>>,
}

/// The app's live channels, by space id (in [`AppState`]).
#[derive(Default)]
pub struct LiveHub {
    channels: Mutex<HashMap<String, Handle>>,
}

impl LiveHub {
    /// Whether any channel is connected (the worker then syncs local edits sooner).
    pub fn any_live(&self) -> bool {
        self.channels.lock().is_ok_and(|c| {
            c.values()
                .any(|h| h.state.lock().is_ok_and(|s| *s == LiveState::Live))
        })
    }

    /// The space ids with a channel.
    pub fn spaces(&self) -> Vec<String> {
        self.channels
            .lock()
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Updates a channel's session token (after the worker renewed it).
    pub fn set_token(&self, space: &str, token: &str) {
        if let Ok(c) = self.channels.lock() {
            if let Some(h) = c.get(space) {
                if let Ok(mut t) = h.token.lock() {
                    if *t != token {
                        *t = token.to_string();
                    }
                }
            }
        }
    }

    /// Records who the members are (display and role), for presence.
    pub fn set_members(&self, space: &str, members: HashMap<String, (String, Role)>) {
        if let Ok(c) = self.channels.lock() {
            if let Some(h) = c.get(space) {
                if let Ok(mut m) = h.members.lock() {
                    *m = members;
                }
            }
        }
    }

    /// Stops a channel.
    pub fn stop(&self, space: &str) {
        if let Ok(mut c) = self.channels.lock() {
            if let Some(h) = c.remove(space) {
                let _ = h.tx.send(Ctl::Stop);
            }
        }
    }

    /// Forgets a channel whose thread is ending on its own.
    fn forget(&self, space: &str) {
        if let Ok(mut c) = self.channels.lock() {
            c.remove(space);
        }
    }

    /// Stops every channel (sign-out, disconnect).
    pub fn stop_all(&self) {
        for s in self.spaces() {
            self.stop(&s);
        }
    }

    fn send(&self, notebook: Uuid, ctl: Ctl) -> bool {
        self.channels.lock().is_ok_and(|c| {
            c.values()
                .find(|h| h.notebook == notebook)
                .is_some_and(|h| h.tx.send(ctl).is_ok())
        })
    }
}

/// What a channel needs to start.
pub struct Start {
    /// Space id.
    pub space: String,
    /// The shared notebook.
    pub notebook: Uuid,
    /// This account (the member).
    pub member: String,
    /// This device's id in the space.
    pub device: String,
    /// Server base URL.
    pub base_url: String,
    /// The space's device session token.
    pub token: String,
    /// The awareness key (from the space root).
    pub key: [u8; 32],
}

/// Starts a channel unless one runs for that space.
pub fn ensure(app: &AppHandle, s: Start) {
    let hub = &app.state::<AppState>().live;
    let Ok(mut channels) = hub.channels.lock() else {
        return;
    };
    if channels.contains_key(&s.space) {
        return;
    }
    let (Ok(space), Ok(member), Ok(device)) = (
        s.space.parse::<AccountId>(),
        s.member.parse::<AccountId>(),
        s.device.parse::<DeviceId>(),
    ) else {
        return;
    };
    let token = Arc::new(Mutex::new(s.token));
    let state = Arc::new(Mutex::new(LiveState::Connecting));
    let members = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel();
    let client = LiveClient::new(
        space,
        member,
        device,
        s.key,
        load_prefs(app).share_presence,
        Box::new(WsDialer {
            base_url: s.base_url,
            token: token.clone(),
        }),
    );
    let ctx = Thread {
        app: app.clone(),
        space: s.space.clone(),
        notebook: s.notebook,
        state: state.clone(),
        members: members.clone(),
    };
    let spawned = std::thread::Builder::new()
        .name("penroam-live".into())
        .spawn(move || ctx.run(client, rx));
    if spawned.is_ok() {
        channels.insert(
            s.space,
            Handle {
                tx,
                notebook: s.notebook,
                token,
                state,
                members,
            },
        );
    }
}

struct Thread {
    app: AppHandle,
    space: String,
    notebook: Uuid,
    state: Arc<Mutex<LiveState>>,
    members: Arc<Mutex<HashMap<String, (String, Role)>>>,
}

/// The order frames go out in when several kinds wait: where, then what is being done there.
fn kind_order(k: AwarenessKind) -> u8 {
    match k {
        AwarenessKind::Here => 0,
        AwarenessKind::Selection => 1,
        AwarenessKind::Caret => 2,
        AwarenessKind::Viewport => 3,
    }
}

/// A stable palette index for a person: the same on every device that sees them.
fn color_index(account: &str) -> u32 {
    let h = penroam_sync::crypto::sha256(account.as_bytes());
    u32::from_le_bytes([h[0], h[1], h[2], h[3]]) % 8
}

impl Thread {
    fn run(self, mut client: LiveClient, rx: Receiver<Ctl>) {
        self.status(LiveState::Connecting);
        let mut presence = load_prefs(&self.app).share_presence;
        // the newest frame of each kind not sent yet (over the rate, a newer frame replaces an
        // older one of its kind instead of queueing)
        let mut pending: BTreeMap<u8, AwarenessPayload> = BTreeMap::new();
        loop {
            loop {
                match rx.try_recv() {
                    Ok(Ctl::Awareness(p)) => {
                        if p.kind == AwarenessKind::Here {
                            // moving to another page makes caret and selection there stale
                            pending.retain(|_, q| q.page_id == p.page_id);
                        }
                        pending.insert(kind_order(p.kind), p);
                    }
                    Ok(Ctl::Presence(on)) => {
                        presence = on;
                        if !on {
                            pending.clear();
                        }
                        client.set_presence(on)
                    }
                    Ok(Ctl::Stop) | Err(TryRecvError::Disconnected) => {
                        client.stop();
                        self.status(LiveState::Disabled);
                        return;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            if !presence || client.state() != LiveState::Live {
                pending.clear();
            }
            while let Some((&k, p)) = pending.iter().next() {
                let Ok(v) = serde_json::to_value(p.wire()) else {
                    pending.remove(&k);
                    continue;
                };
                match client.send_awareness(&v) {
                    Ok(true) | Err(_) => {
                        pending.remove(&k);
                    }
                    // over the rate: try again in a moment, with whatever is newest by then
                    Ok(false) => break,
                }
            }
            let wait = if pending.is_empty() { 250 } else { 30 };
            for ev in client.step(Duration::from_millis(wait)) {
                match ev {
                    LiveEvent::State(s) => self.status(s),
                    LiveEvent::CatchUp(_) => crate::sync::hint_space(&self.app, &self.space),
                    LiveEvent::CommentsCatchUp(_) => {
                        crate::sync::hint_comments(&self.app, &self.space)
                    }
                    LiveEvent::Notification => {
                        let _ = self.app.emit("notifications://changed", ());
                    }
                    LiveEvent::Peers(p) => self.presence(&p),
                    LiveEvent::Membership { .. } => {
                        // a role change: sync now, which rereads the list and the role
                        crate::sync::hint_space(&self.app, &self.space)
                    }
                    LiveEvent::AccessRemoved => {
                        // a cue, not proof: the worker confirms over HTTP and restarts the channel
                        // if access is still there
                        let hub = &self.app.state::<AppState>().live;
                        hub.forget(&self.space);
                        self.status(LiveState::Offline);
                        crate::sync::revoke_space(&self.app, &self.space);
                        return;
                    }
                }
            }
        }
    }

    fn status(&self, s: LiveState) {
        if let Ok(mut st) = self.state.lock() {
            *st = s;
        }
        let _ = self.app.emit(
            "live://status",
            LiveStatusEvent {
                notebook_id: self.notebook,
                state: s.into(),
            },
        );
    }

    fn presence(&self, peers: &[Peer]) {
        let members = self.members.lock().map(|m| m.clone()).unwrap_or_default();
        let now = Instant::now();
        let wall = penroam_core::now_ms().max(0) as u64;
        let out: Vec<PresencePeer> = peers
            .iter()
            .filter_map(|p| {
                let w: Wire = serde_json::from_value(p.payload.clone()).ok()?;
                // the same bounds as for what this device sends: anything else is dropped
                let checked = AwarenessPayload {
                    page_id: w.page_id,
                    kind: w.kind,
                    rect: w.rect,
                    caret: w.caret,
                    block_id: w.block_id,
                };
                checked.check().ok()?;
                let (display, role) = members
                    .get(p.member.as_str())
                    .cloned()
                    .unwrap_or_else(|| (String::new(), Role::Viewer));
                Some(PresencePeer {
                    session_id: p.session_id.clone(),
                    member_display: display,
                    role: role.into(),
                    page_id: w.page_id,
                    kind: w.kind,
                    rect: w.rect,
                    caret: w.caret,
                    block_id: w.block_id,
                    color_index: color_index(p.member.as_str()),
                    expires_at: wall + p.expires.saturating_duration_since(now).as_millis() as u64,
                })
            })
            .collect();
        let _ = self.app.emit(
            "live://presence",
            PresenceEvent {
                notebook_id: self.notebook,
                peers: out,
            },
        );
    }
}

fn cmd_err(m: impl Into<String>) -> CmdError {
    CmdError {
        code: ErrorCode::Invalid,
        message: m.into(),
    }
}

/// Tells the others in a shared notebook where this person is. Dropped quietly when the
/// notebook has no live channel, presence is off, or it is sent too often.
#[tauri::command(async)]
pub fn live_awareness(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    payload: AwarenessPayload,
) -> Result<(), CmdError> {
    payload.check().map_err(cmd_err)?;
    state.live.send(notebook_id, Ctl::Awareness(payload));
    Ok(())
}

/// The presence preference; with `prefs`, sets it first (and tells the running channels).
#[tauri::command(async)]
pub fn live_prefs(
    app: AppHandle,
    state: State<'_, AppState>,
    prefs: Option<LivePrefs>,
) -> Result<LivePrefs, CmdError> {
    if let Some(p) = prefs {
        let path = prefs_path(&app).ok_or_else(|| cmd_err("no settings folder"))?;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| cmd_err(e.to_string()))?;
        }
        penroam_storage::fsutil::write_atomic(
            &path,
            &serde_json::to_vec(&p).map_err(|e| cmd_err(e.to_string()))?,
        )
        .map_err(|e| cmd_err(e.to_string()))?;
        if let Ok(c) = state.live.channels.lock() {
            for h in c.values() {
                let _ = h.tx.send(Ctl::Presence(p.share_presence));
            }
        }
    }
    Ok(load_prefs(&app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awareness_is_checked_and_named_as_the_contract_says() {
        let p = AwarenessPayload {
            page_id: Uuid::from_u128(1),
            kind: AwarenessKind::Caret,
            rect: None,
            caret: Some(PageCaret {
                x: 1.0,
                y: 2.0,
                h: 14.0,
            }),
            block_id: Some(Uuid::from_u128(2)),
        };
        assert!(p.check().is_ok());
        let w = serde_json::to_value(p.wire()).unwrap();
        assert!(w.get("page_id").is_some() && w.get("block_id").is_some());
        assert_eq!(w["kind"], "caret");
        let bad = AwarenessPayload {
            caret: Some(PageCaret {
                x: f64::NAN,
                y: 0.0,
                h: 1.0,
            }),
            ..p.clone()
        };
        assert!(bad.check().is_err());
        // the editor's camelCase shape is what the command takes
        let ui: AwarenessPayload = serde_json::from_value(serde_json::json!({
            "pageId": Uuid::from_u128(1), "kind": "selection",
            "rect": {"x": 0, "y": 0, "w": 10, "h": 5}
        }))
        .unwrap();
        assert_eq!(ui.kind, AwarenessKind::Selection);
        assert_eq!(
            serde_json::to_value(LivePrefs::default()).unwrap(),
            serde_json::json!({"sharePresence": true})
        );
        assert!(color_index("someone") < 8);
        assert_eq!(color_index("someone"), color_index("someone"));
    }
}
