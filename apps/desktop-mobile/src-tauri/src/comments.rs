//! Comments and mentions in shared notebooks, for the app: the `comments_*`, `notifications_*`
//! and `notification_prefs*` commands, their views, and the `comments://changed` and
//! `notifications://changed` events.
//!
//! The comment stream of each space lives in `penroam_sync::comments` (one `CommentSync` per
//! space, held by the worker's `SpaceSet`, synced after the space's document stream). Commands are
//! sent to the sync worker ([`CommentCmd`]) because the worker owns the streams. Writes return once
//! the change is durable on this device; the UI reloads [`comments_list`] and reads each comment's
//! `delivery` (sent, waiting, failed, draft).
//!
//! Anchors are placed with `penroam_core::comment::resolve` on the notebook as it is on this
//! device; text anchors go through the space's text CRDT (Yrs sticky indexes), so they follow the
//! text while others edit it and become orphaned rather than jumping when it is gone.
//!
//! Names shown for people come only from the space's verified member list (never from a comment).
//! A retried comment is sent as a new one; the failed one it replaces is dropped
//! ([`CommentSync::discard`](penroam_sync::comments::CommentSync::discard)).

use crate::commands::CmdError;
use crate::AppState;
use penroam_core::comment::{
    parse_mentions, quote_hash, resolve, CommentAnchor, CommentOp, CommentState, CommentThread,
    OrphanReason, Rect, Resolved, ThreadStatus,
};
use penroam_core::{Element, Page};
use penroam_storage::ErrorCode;
use penroam_sync::comments::{
    CommentIntent, CommentSync, Delivery, NotificationCheck, SpaceRoster,
};
use penroam_sync::shared::{SpaceAccess, SpaceSet};
use penroam_sync::transport::HttpTransport;
use penroam_sync::vault::NotebookOwners;
use penroam_sync::SyncEngine;
use penroam_sync_protocol::comments::{MutedThread, NotificationKind, NotificationPrefs};
use penroam_sync_protocol::{MembershipState, NotificationId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::Sender;
use tauri::{AppHandle, Emitter, Manager, State};
use ts_rs::TS;
use uuid::Uuid;

type Engine = SyncEngine<HttpTransport>;
type Reply<T> = Sender<Result<T, String>>;

/// A rectangle in page coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, TS)]
#[ts(export, rename = "CommentRect")]
pub struct RectView {
    /// Left.
    pub x: f64,
    /// Top.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl From<Rect> for RectView {
    fn from(r: Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

/// Where a thread's anchor is now.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum PlacementView {
    /// On a page.
    #[serde(rename_all = "camelCase")]
    Placed {
        /// The page.
        page_id: Uuid,
        /// Where on it.
        rect: RectView,
    },
    /// Its target is gone; listed with "locate" and "reattach".
    #[serde(rename_all = "camelCase")]
    Orphaned {
        /// Why: `elementDeleted`, `pageDeleted`, `textChanged` or `missing`.
        reason: String,
        /// The page it was on.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        last_page_id: Option<Uuid>,
        /// The quoted text of a text anchor.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        quote: Option<String>,
    },
}

/// Whether a comment reached the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DeliveryView {
    /// Stored by the server.
    Sent,
    /// Queued.
    Waiting,
    /// Refused; send it again.
    Failed,
    /// Kept here only (commenting not allowed).
    Draft,
}

impl From<Delivery> for DeliveryView {
    fn from(d: Delivery) -> Self {
        match d {
            Delivery::Sent => Self::Sent,
            Delivery::Waiting => Self::Waiting,
            Delivery::Failed => Self::Failed,
            Delivery::Draft => Self::Draft,
        }
    }
}

/// One comment.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CommentView {
    /// Its id.
    pub comment_id: Uuid,
    /// Who wrote it (verified).
    pub author_account_id: String,
    /// Their verified email from the member list; empty when unknown.
    pub author_display: String,
    /// Plain text with `@[accountId]` mention tokens; empty once deleted.
    pub body: String,
    /// Account ids mentioned.
    pub mentions: Vec<String>,
    /// When it was written (Unix ms).
    #[ts(type = "number")]
    pub at: i64,
    /// Edited since.
    pub edited: bool,
    /// Deleted by its author.
    pub deleted: bool,
    /// Removed by the notebook's owner.
    pub removed_by_owner: bool,
    /// Whether it reached the server.
    pub delivery: DeliveryView,
}

/// One thread.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadView {
    /// Its id.
    pub thread_id: Uuid,
    /// What it is attached to (the anchor's tagged JSON).
    #[ts(type = "Record<string, unknown>")]
    pub anchor: serde_json::Value,
    /// Where that is now.
    pub placement: PlacementView,
    /// `open` or `resolved`.
    pub status: String,
    /// Who resolved it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resolved_by_display: Option<String>,
    /// Its comments, oldest first.
    pub comments: Vec<CommentView>,
    /// An unread notification points into it.
    pub unread: bool,
    /// This person started it.
    pub mine: bool,
    /// A comment in it mentions this person.
    pub mentions_me: bool,
}

/// `comments_list`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CommentsView {
    /// This person may write comments.
    pub can_comment: bool,
    /// This person owns the notebook (may remove others' comments).
    pub is_moderator: bool,
    /// Every thread, oldest first.
    pub threads: Vec<ThreadView>,
}

/// Someone who can be mentioned.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MentionableView {
    /// Their account id (the `@[accountId]` token).
    pub account_id: String,
    /// Their verified email.
    pub display: String,
}

/// One notification.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NotificationView {
    /// Its id.
    pub notification_id: String,
    /// The shared notebook.
    pub notebook_id: Uuid,
    /// The thread.
    pub thread_id: Uuid,
    /// The comment.
    pub comment_id: Uuid,
    /// `mention` or `reply`.
    pub kind: String,
    /// Who did it; empty when unknown.
    pub actor_display: String,
    /// When (Unix ms).
    #[ts(type = "number")]
    pub at: u64,
    /// Read already.
    pub read: bool,
}

/// `notifications_list`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NotificationsView {
    /// Newest last.
    pub items: Vec<NotificationView>,
    /// How many are unread.
    #[ts(type = "number")]
    pub unread: u64,
}

/// A muted thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MutedThreadView {
    /// Its notebook.
    pub notebook_id: Uuid,
    /// The thread.
    pub thread_id: Uuid,
}

/// Notification mutes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NotificationPrefsView {
    /// Shared notebooks muted entirely.
    pub muted_notebooks: Vec<Uuid>,
    /// Threads muted.
    pub muted_threads: Vec<MutedThreadView>,
}

/// Requests for the sync worker.
pub enum CommentCmd {
    /// [`comments_list`].
    List(Uuid, Reply<CommentsView>),
    /// [`comments_text_anchor`].
    TextAnchor(Uuid, Uuid, Uuid, u32, u32, Reply<serde_json::Value>),
    /// A write (create, reply, edit, delete, resolve, reattach); answers the thread it touched.
    Write(Uuid, Write, Reply<Option<ThreadView>>),
    /// [`comments_mentionable`].
    Mentionable(Uuid, Reply<Vec<MentionableView>>),
    /// [`notifications_list`].
    Notifications(Reply<NotificationsView>),
    /// [`notifications_mark_read`].
    MarkRead(String, Reply<()>),
    /// [`notification_prefs`] (with `Some`, set first).
    Prefs(Option<NotificationPrefsView>, Reply<NotificationPrefsView>),
    /// The comment threads of a notebook as JSON, for an export with comments.
    Export(Uuid, Reply<Option<Vec<u8>>>),
}

/// A change to the comments.
pub enum Write {
    /// Start a thread.
    Create(CommentAnchor, String),
    /// Reply.
    Reply(Uuid, String, Option<Uuid>),
    /// Edit own comment.
    Edit(Uuid, String),
    /// Delete own comment (or, owner, remove anyone's).
    Delete(Uuid),
    /// Resolve or reopen.
    SetResolved(Uuid, bool),
    /// Attach elsewhere.
    Reattach(Uuid, CommentAnchor),
}

fn space_of(app: &AppHandle, nb: Uuid) -> Result<String, String> {
    let state = app.state::<AppState>();
    let root = state
        .lib
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .root()
        .to_path_buf();
    NotebookOwners::load(&root)
        .map_err(|e| e.to_string())?
        .shared_entry(&nb)
        .map(|e| e.space_id.clone())
        .ok_or_else(|| "this notebook isn't shared".to_string())
}

fn orphan_reason(r: OrphanReason) -> &'static str {
    match r {
        OrphanReason::ElementDeleted => "elementDeleted",
        OrphanReason::PageTrashed | OrphanReason::PageDeleted => "pageDeleted",
        OrphanReason::TextRangeLost | OrphanReason::QuoteMismatch => "textChanged",
        OrphanReason::PageUnavailable | OrphanReason::PdfPageMissing => "missing",
    }
}

type People = HashMap<String, (String, bool, penroam_sync_protocol::Role)>;

/// Members of a space (account id → verified display, active, role) and the roster, read from
/// the server once; offline (or when the server can't be reached) the last list read on this
/// device. Both are empty/`None` when no list was ever read here. Never fails on the network.
fn people_and_roster(
    account: &mut Engine,
    spaces: &mut SpaceSet<HttpTransport>,
    space: &str,
) -> Result<(People, Option<SpaceRoster>), String> {
    let roster = spaces.roster(account, space).map_err(sync_msg)?;
    let people = spaces
        .cached_members(space)
        .map(|l| {
            l.members
                .iter()
                .map(|m| {
                    (
                        m.account_id.to_string(),
                        (
                            m.display.clone(),
                            m.state == MembershipState::Active,
                            m.role,
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((people, roster))
}

/// Builds the threads of a notebook for the UI.
fn build_view(
    app: &AppHandle,
    account: &mut Engine,
    spaces: &mut SpaceSet<HttpTransport>,
    nb: Uuid,
    space: &str,
) -> Result<CommentsView, String> {
    let state = app.state::<AppState>();
    let lib = &state.lib;
    let (people, roster) = people_and_roster(account, spaces, space)?;
    let me = account.account_id().to_string();
    let display = |a: &str| people.get(a).map(|p| p.0.clone()).unwrap_or_default();
    if spaces
        .comment_stream(account, lib, space)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err("this shared notebook isn't syncing on this device".into());
    }
    let cs: &CommentSync = spaces.comments.get(space).expect("opened");
    // only notifications backed by a verified comment mark a thread unread
    let unread_threads: Vec<Uuid> = spaces
        .notifications
        .iter()
        .filter(|n| !n.read && n.space_id.as_str() == space)
        .filter(|n| cs.check_notification(n) == NotificationCheck::Genuine)
        .filter_map(|n| Uuid::parse_str(n.thread_id.as_str()).ok())
        .collect();
    let mut threads: Vec<CommentThread> = cs.threads();
    let unsent = cs.unsent();
    // a failed (or draft) new thread is still shown, with its first comment marked
    for u in &unsent {
        if let CommentOp::ThreadCreated {
            thread_id,
            anchor,
            body,
        } = &u.op
        {
            if threads.iter().all(|t| t.thread_id != *thread_id) {
                threads.push(CommentThread {
                    thread_id: *thread_id,
                    anchor: anchor.clone(),
                    anchor_alternates: Vec::new(),
                    created_by: me.clone(),
                    created_at: u.at,
                    status: ThreadStatus::Open,
                    status_change: None,
                    comments: vec![penroam_core::comment::Comment {
                        comment_id: *thread_id,
                        thread_id: *thread_id,
                        author: me.clone(),
                        reply_to: None,
                        created_at: u.at,
                        state: CommentState::Visible,
                        deleted_at: None,
                        body: Some(body.clone()),
                        mentions: parse_mentions(body),
                        edited: false,
                        edited_at: None,
                        alternate_bodies: Vec::new(),
                    }],
                });
            }
        }
    }
    let unsent_replies: Vec<(Uuid, CommentView)> = unsent
        .iter()
        .filter_map(|u| match &u.op {
            CommentOp::Replied {
                thread_id,
                comment_id,
                body,
                ..
            } => Some((
                *thread_id,
                CommentView {
                    comment_id: *comment_id,
                    author_account_id: me.clone(),
                    author_display: display(&me),
                    body: body.clone(),
                    mentions: parse_mentions(body),
                    at: u.at,
                    edited: false,
                    deleted: false,
                    removed_by_owner: false,
                    delivery: u.delivery.into(),
                },
            )),
            _ => None,
        })
        .collect();
    let deliveries: HashMap<Uuid, DeliveryView> = threads
        .iter()
        .flat_map(|t| t.comments.iter())
        .map(|c| (c.comment_id, cs.comment_delivery(c.comment_id).into()))
        .collect();
    // place anchors on the notebook as it is here; text anchors through the text CRDT
    let notebook = lib
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .notebook(nb)
        .map_err(|e| e.to_string())?
        .clone();
    let mut pages: Vec<Page> = Vec::new();
    {
        let mut l = lib.lock().unwrap_or_else(|p| p.into_inner());
        let mut want: Vec<Uuid> = threads.iter().map(|t| t.anchor.page_id()).collect();
        want.sort();
        want.dedup();
        for p in want {
            if notebook.all_page_ids().any(|x| x == p) {
                if let Ok(page) = l.load_page(nb, p) {
                    pages.push(page);
                }
            }
        }
    }
    let engine = spaces
        .engine(space)
        .ok_or("this shared notebook isn't syncing on this device")?;
    let replica = engine.replica_of(nb).map_err(|e| e.to_string())?;
    let offset =
        |page: Uuid, element: Uuid, sticky: &[u8]| replica.text_offset(page, element, sticky);
    let mut out = Vec::new();
    for t in threads {
        let placement = match resolve(&t.anchor, &notebook, &pages, &offset) {
            Resolved::Placed { page_id, rect, .. } => PlacementView::Placed {
                page_id,
                rect: rect.into(),
            },
            Resolved::Orphaned {
                reason,
                last_known_page,
                quote,
            } => PlacementView::Orphaned {
                reason: orphan_reason(reason).into(),
                last_page_id: Some(last_known_page),
                quote,
            },
        };
        let mut comments: Vec<CommentView> = t
            .comments
            .iter()
            .map(|c| CommentView {
                comment_id: c.comment_id,
                author_account_id: c.author.clone(),
                author_display: display(&c.author),
                body: c.body.clone().unwrap_or_default(),
                mentions: c.mentions.clone(),
                at: c.created_at,
                edited: c.edited,
                deleted: c.state == CommentState::Deleted,
                removed_by_owner: c.state == CommentState::RemovedByOwner,
                delivery: deliveries
                    .get(&c.comment_id)
                    .copied()
                    .unwrap_or(DeliveryView::Sent),
            })
            .collect();
        comments.extend(
            unsent_replies
                .iter()
                .filter(|(th, _)| *th == t.thread_id)
                .map(|(_, c)| c.clone()),
        );
        let mentions_me = comments.iter().any(|c| c.mentions.contains(&me));
        out.push(ThreadView {
            thread_id: t.thread_id,
            anchor: serde_json::to_value(&t.anchor).unwrap_or_default(),
            placement,
            status: match t.status {
                ThreadStatus::Open => "open".into(),
                ThreadStatus::Resolved => "resolved".into(),
            },
            resolved_by_display: match (&t.status, &t.status_change) {
                (ThreadStatus::Resolved, Some(c)) => Some(display(&c.by)),
                _ => None,
            },
            unread: unread_threads.contains(&t.thread_id),
            mine: t.created_by == me,
            mentions_me,
            comments,
        });
    }
    out.sort_by_key(|t| t.comments.first().map(|c| c.at).unwrap_or(0));
    Ok(CommentsView {
        can_comment: roster.as_ref().is_some_and(|r| r.may_comment()),
        is_moderator: roster.as_ref().is_some_and(|r| r.is_owner()),
        threads: out,
    })
}

/// A text anchor for UTF-16 range `start..end` of a text block, with sticky positions, the quoted
/// text and its hash, and the block's rectangle as a fallback.
fn text_anchor(
    app: &AppHandle,
    spaces: &mut SpaceSet<HttpTransport>,
    space: &str,
    (nb, page, element): (Uuid, Uuid, Uuid),
    (start, end): (u32, u32),
) -> Result<serde_json::Value, String> {
    if start > end {
        return Err("the range ends before it starts".into());
    }
    let block = {
        let state = app.state::<AppState>();
        let mut l = state.lib.lock().unwrap_or_else(|p| p.into_inner());
        let p = l.load_page(nb, page).map_err(|e| e.to_string())?;
        match p.elements.into_iter().find(|e| e.id() == element) {
            Some(Element::Text(t)) => t,
            _ => return Err("not a text block".into()),
        }
    };
    let quote = {
        let s16: Vec<u16> = block.text.encode_utf16().collect();
        if end as usize > s16.len() {
            return Err("the range is past the end of the text".into());
        }
        String::from_utf16(&s16[start as usize..end as usize])
            .map_err(|_| "the range splits a character".to_string())?
    };
    let engine = spaces
        .engine(space)
        .ok_or("this shared notebook isn't syncing on this device")?;
    let rep = engine.replica_of(nb).map_err(|e| e.to_string())?;
    let (Some(s), Some(e)) = (
        rep.text_sticky(page, element, start, true),
        rep.text_sticky(page, element, end, false),
    ) else {
        return Err("this text can't be anchored yet; try again after it syncs".into());
    };
    let anchor = CommentAnchor::TextRange {
        page_id: page,
        element_id: element,
        start: s,
        end: e,
        quote_hash: quote_hash(&quote),
        quote,
        fallback_rect: Some(Rect {
            x: block.x,
            y: block.y,
            w: block.width,
            h: block.height,
        }),
    };
    anchor.validate().map_err(|e| e.to_string())?;
    serde_json::to_value(anchor).map_err(|e| e.to_string())
}

fn sync_msg(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Runs one [`CommentCmd`] on the worker thread. Returns the notebook whose comments changed, if
/// any (the worker emits `comments://changed`), and whether notifications changed.
pub fn handle(
    app: &AppHandle,
    account: &mut Engine,
    spaces: &mut SpaceSet<HttpTransport>,
    access: &mut SpaceAccess<'_, HttpTransport>,
    cmd: CommentCmd,
) -> (Option<Uuid>, bool) {
    let state = app.state::<AppState>();
    let lib = &state.lib;
    let _ = access;
    match cmd {
        CommentCmd::List(nb, r) => {
            let res =
                space_of(app, nb).and_then(|space| build_view(app, account, spaces, nb, &space));
            let _ = r.send(res);
            (None, false)
        }
        CommentCmd::TextAnchor(nb, page, element, start, end, r) => {
            let res = space_of(app, nb).and_then(|space| {
                text_anchor(app, spaces, &space, (nb, page, element), (start, end))
            });
            let _ = r.send(res);
            (None, false)
        }
        CommentCmd::Write(nb, w, r) => {
            let res = (|| -> Result<Option<ThreadView>, String> {
                let space = space_of(app, nb)?;
                // Offline with no member list saved here the role is unknown: the comment is
                // queued anyway, and the server and the next sync decide (a refusal turns it into
                // a failed comment, a lost permission into a draft).
                let (may_comment, is_owner) =
                    match spaces.roster(account, &space).map_err(sync_msg)? {
                        Some(r) => (r.may_comment(), r.is_owner()),
                        None => (true, false),
                    };
                let cs = spaces
                    .comment_stream(account, lib, &space)
                    .map_err(sync_msg)?
                    .ok_or("this shared notebook isn't syncing on this device")?;
                // a comment sent again replaces the failed one with the same text
                let resend_of = |thread: Option<Uuid>, body: &str| {
                    cs.unsent()
                        .into_iter()
                        .filter(|u| u.delivery == Delivery::Failed)
                        .filter(|u| match (&u.op, thread) {
                            (
                                CommentOp::Replied {
                                    thread_id, body: b, ..
                                },
                                Some(t),
                            ) => *thread_id == t && b.trim() == body.trim(),
                            (CommentOp::ThreadCreated { body: b, .. }, None) => {
                                b.trim() == body.trim()
                            }
                            _ => false,
                        })
                        .map(|u| u.op_id)
                        .collect::<Vec<_>>()
                };
                let (intent, thread) = match w {
                    Write::Create(anchor, body) => {
                        for op in resend_of(None, &body) {
                            let _ = cs.discard(op);
                        }
                        (CommentIntent::CreateThread { anchor, body }, None)
                    }
                    Write::Reply(thread_id, body, reply_to) => {
                        for op in resend_of(Some(thread_id), &body) {
                            let _ = cs.discard(op);
                        }
                        (
                            CommentIntent::Reply {
                                thread_id,
                                body,
                                reply_to,
                            },
                            Some(thread_id),
                        )
                    }
                    Write::Edit(comment_id, body) => {
                        (CommentIntent::Edit { comment_id, body }, None)
                    }
                    Write::Delete(comment_id) => (CommentIntent::Delete { comment_id }, None),
                    Write::SetResolved(thread_id, resolved) => (
                        CommentIntent::SetResolved {
                            thread_id,
                            resolved,
                        },
                        Some(thread_id),
                    ),
                    Write::Reattach(thread_id, anchor) => (
                        CommentIntent::Reattach { thread_id, anchor },
                        Some(thread_id),
                    ),
                };
                let created = matches!(intent, CommentIntent::CreateThread { .. });
                let id = cs
                    .submit(intent, may_comment, is_owner, penroam_core::now_ms())
                    .map_err(sync_msg)?;
                // upload now (best effort; it stays queued otherwise)
                let _ = spaces.sync_comments(account, lib, &space);
                let thread = if created { Some(id) } else { thread };
                Ok(match thread {
                    Some(t) => build_view(app, account, spaces, nb, &space)?
                        .threads
                        .into_iter()
                        .find(|v| v.thread_id == t),
                    None => None,
                })
            })();
            let changed = res.is_ok().then_some(nb);
            let _ = r.send(res);
            (changed, false)
        }
        CommentCmd::Mentionable(nb, r) => {
            let me = account.account_id().to_string();
            let res = space_of(app, nb)
                .and_then(|space| people_and_roster(account, spaces, &space))
                .map(|(m, _)| {
                    let mut v: Vec<MentionableView> = m
                        .into_iter()
                        .filter(|(id, (_, active, _))| *active && *id != me)
                        .map(|(account_id, (display, _, _))| MentionableView {
                            account_id,
                            display,
                        })
                        .collect();
                    v.sort_by(|a, b| a.display.cmp(&b.display));
                    v
                });
            let _ = r.send(res);
            (None, false)
        }
        CommentCmd::Notifications(r) => {
            let res = notifications_view(app, account, spaces);
            let _ = r.send(res);
            (None, false)
        }
        CommentCmd::MarkRead(id, r) => {
            let res = id
                .parse::<NotificationId>()
                .map_err(|_| "not a notification id".to_string())
                .and_then(|id| {
                    penroam_sync::comments::mark_notification(account.transport_mut(), &id, true)
                        .map_err(sync_msg)
                })
                .map(|rec| {
                    if let Some(n) = spaces
                        .notifications
                        .iter_mut()
                        .find(|n| n.notification_id == rec.notification_id)
                    {
                        n.read = true;
                    }
                });
            let _ = r.send(res);
            (None, true)
        }
        CommentCmd::Prefs(set, r) => {
            let res = prefs(app, account, set);
            let _ = r.send(res);
            (None, false)
        }
        CommentCmd::Export(nb, r) => {
            let res = space_of(app, nb).and_then(|space| {
                let cs = spaces
                    .comment_stream(account, lib, &space)
                    .map_err(sync_msg)?
                    .ok_or("this shared notebook isn't syncing on this device")?;
                let doc = serde_json::json!({
                    "format": "penroam-comments",
                    "version": 1,
                    "notebookId": nb,
                    "threads": cs.threads(),
                });
                serde_json::to_vec_pretty(&doc)
                    .map(Some)
                    .map_err(|e| e.to_string())
            });
            let _ = r.send(res);
            (None, false)
        }
    }
}

/// Space → notebook, for every shared notebook here.
fn notebooks_by_space(app: &AppHandle) -> HashMap<String, Uuid> {
    let state = app.state::<AppState>();
    let root = state
        .lib
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .root()
        .to_path_buf();
    NotebookOwners::load(&root)
        .map(|o| {
            o.shared
                .iter()
                .map(|(nb, e)| (e.space_id.clone(), *nb))
                .collect()
        })
        .unwrap_or_default()
}

fn notifications_view(
    app: &AppHandle,
    account: &mut Engine,
    spaces: &mut SpaceSet<HttpTransport>,
) -> Result<NotificationsView, String> {
    let (all, _, _) =
        penroam_sync::comments::notifications(account.transport_mut(), None).map_err(sync_msg)?;
    let by_space = notebooks_by_space(app);
    let state = app.state::<AppState>();
    let mut displays: HashMap<String, People> = HashMap::new();
    let mut verified = Vec::new();
    let mut items = Vec::new();
    for n in all {
        let space = n.space_id.to_string();
        // notifications of notebooks not on this device (or not shared any more) are left out
        let Some(nb) = by_space.get(&space) else {
            continue;
        };
        if !displays.contains_key(&space) {
            let m = people_and_roster(account, spaces, &space)
                .map(|(p, _)| p)
                .unwrap_or_default();
            displays.insert(space.clone(), m);
        }
        // the server builds notifications from unsigned data: show only those a verified
        // comment backs (a forged one is recorded as a security event by the stream)
        let Ok(Some(cs)) = spaces.comment_stream(account, &state.lib, &space) else {
            continue;
        };
        if !cs.verify_notification(&n) {
            continue;
        }
        let (Ok(thread), Ok(op)) = (
            Uuid::parse_str(n.thread_id.as_str()),
            Uuid::parse_str(n.comment_op_id.as_str()),
        ) else {
            continue;
        };
        // an edit that added a mention names the edit; the UI wants the comment
        let comment = cs.comment_of_op(op).unwrap_or(op);
        let actor = displays[&space]
            .get(n.actor.as_str())
            .map(|p| p.0.clone())
            .unwrap_or_default();
        verified.push(n.clone());
        items.push(NotificationView {
            notification_id: n.notification_id.to_string(),
            notebook_id: *nb,
            thread_id: thread,
            comment_id: comment,
            kind: match n.kind {
                NotificationKind::Mention => "mention".into(),
                NotificationKind::Reply => "reply".into(),
                NotificationKind::Unknown => continue,
            },
            actor_display: actor,
            at: n.created_at_ms,
            read: n.read,
        });
    }
    spaces.notifications = verified;
    let unread = items.iter().filter(|i| !i.read).count() as u64;
    Ok(NotificationsView { items, unread })
}

fn prefs(
    app: &AppHandle,
    account: &mut Engine,
    set: Option<NotificationPrefsView>,
) -> Result<NotificationPrefsView, String> {
    let by_space = notebooks_by_space(app);
    let by_nb: BTreeMap<Uuid, String> = by_space.iter().map(|(s, n)| (*n, s.clone())).collect();
    let saved = match set {
        Some(p) => {
            let wire = NotificationPrefs {
                muted_spaces: p
                    .muted_notebooks
                    .iter()
                    .filter_map(|nb| by_nb.get(nb).and_then(|s| s.parse().ok()))
                    .collect(),
                muted_threads: p
                    .muted_threads
                    .iter()
                    .filter_map(|m| {
                        Some(MutedThread {
                            space_id: by_nb.get(&m.notebook_id)?.parse().ok()?,
                            thread_id: m.thread_id.to_string().parse().ok()?,
                        })
                    })
                    .collect(),
            };
            penroam_sync::comments::set_notification_prefs(account.transport_mut(), &wire)
                .map_err(sync_msg)?
        }
        None => {
            penroam_sync::comments::notification_prefs(account.transport_mut()).map_err(sync_msg)?
        }
    };
    Ok(NotificationPrefsView {
        muted_notebooks: saved
            .muted_spaces
            .iter()
            .filter_map(|s| by_space.get(s.as_str()).copied())
            .collect(),
        muted_threads: saved
            .muted_threads
            .iter()
            .filter_map(|m| {
                Some(MutedThreadView {
                    notebook_id: *by_space.get(m.space_id.as_str())?,
                    thread_id: Uuid::parse_str(m.thread_id.as_str()).ok()?,
                })
            })
            .collect(),
    })
}

fn cmd_err(m: impl Into<String>) -> CmdError {
    CmdError {
        code: ErrorCode::Invalid,
        message: m.into(),
    }
}

fn ask<T>(
    state: &State<'_, AppState>,
    make: impl FnOnce(Reply<T>) -> CommentCmd,
) -> Result<T, CmdError> {
    crate::sync::ask_comments(&state.sync, make)
}

/// The comment threads of a shared notebook, placed on its pages.
#[tauri::command(async)]
pub fn comments_list(
    state: State<'_, AppState>,
    notebook_id: Uuid,
) -> Result<CommentsView, CmdError> {
    ask(&state, |r| CommentCmd::List(notebook_id, r))
}

/// A text anchor for UTF-16 offsets `start..end` of a text block.
#[tauri::command(async)]
pub fn comments_text_anchor(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    element_id: Uuid,
    start: u32,
    end: u32,
) -> Result<serde_json::Value, CmdError> {
    ask(&state, |r| {
        CommentCmd::TextAnchor(notebook_id, page_id, element_id, start, end, r)
    })
}

fn parse_anchor(anchor: serde_json::Value) -> Result<CommentAnchor, CmdError> {
    let a: CommentAnchor = serde_json::from_value(anchor).map_err(|e| cmd_err(e.to_string()))?;
    a.validate().map_err(|e| cmd_err(e.to_string()))?;
    Ok(a)
}

/// Starts a thread.
#[tauri::command(async)]
pub fn comments_create_thread(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    anchor: serde_json::Value,
    body: String,
) -> Result<ThreadView, CmdError> {
    let anchor = parse_anchor(anchor)?;
    let t = write(&app, &state, notebook_id, Write::Create(anchor, body))?;
    t.ok_or_else(|| cmd_err("the new thread isn't listed"))
}

fn write(
    app: &AppHandle,
    state: &State<'_, AppState>,
    nb: Uuid,
    w: Write,
) -> Result<Option<ThreadView>, CmdError> {
    let r = ask(state, |r| CommentCmd::Write(nb, w, r))?;
    let _ = app.emit(
        "comments://changed",
        serde_json::json!({ "notebookId": nb }),
    );
    Ok(r)
}

/// Replies in a thread.
#[tauri::command(async)]
pub fn comments_reply(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    thread_id: Uuid,
    body: String,
    reply_to: Option<Uuid>,
) -> Result<(), CmdError> {
    write(
        &app,
        &state,
        notebook_id,
        Write::Reply(thread_id, body, reply_to),
    )
    .map(|_| ())
}

/// Edits one's own comment.
#[tauri::command(async)]
pub fn comments_edit(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    comment_id: Uuid,
    body: String,
) -> Result<(), CmdError> {
    write(&app, &state, notebook_id, Write::Edit(comment_id, body)).map(|_| ())
}

/// Deletes one's own comment (the owner may remove anyone's).
#[tauri::command(async)]
pub fn comments_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    comment_id: Uuid,
) -> Result<(), CmdError> {
    write(&app, &state, notebook_id, Write::Delete(comment_id)).map(|_| ())
}

/// Resolves or reopens a thread.
#[tauri::command(async)]
pub fn comments_set_resolved(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    thread_id: Uuid,
    resolved: bool,
) -> Result<(), CmdError> {
    write(
        &app,
        &state,
        notebook_id,
        Write::SetResolved(thread_id, resolved),
    )
    .map(|_| ())
}

/// Attaches a thread to something else.
#[tauri::command(async)]
pub fn comments_reattach(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    thread_id: Uuid,
    anchor: serde_json::Value,
) -> Result<(), CmdError> {
    let anchor = parse_anchor(anchor)?;
    write(
        &app,
        &state,
        notebook_id,
        Write::Reattach(thread_id, anchor),
    )
    .map(|_| ())
}

/// Active members who can be mentioned (not oneself).
#[tauri::command(async)]
pub fn comments_mentionable(
    state: State<'_, AppState>,
    notebook_id: Uuid,
) -> Result<Vec<MentionableView>, CmdError> {
    ask(&state, |r| CommentCmd::Mentionable(notebook_id, r))
}

/// This account's notifications (mentions and replies), in-app only.
#[tauri::command(async)]
pub fn notifications_list(state: State<'_, AppState>) -> Result<NotificationsView, CmdError> {
    if !state.sync.running() {
        return Ok(NotificationsView {
            items: Vec::new(),
            unread: 0,
        });
    }
    ask(&state, CommentCmd::Notifications)
}

/// Marks a notification read.
#[tauri::command(async)]
pub fn notifications_mark_read(
    app: AppHandle,
    state: State<'_, AppState>,
    notification_id: String,
) -> Result<(), CmdError> {
    ask(&state, |r| CommentCmd::MarkRead(notification_id, r))?;
    let _ = app.emit("notifications://changed", ());
    Ok(())
}

/// The notification mutes.
#[tauri::command(async)]
pub fn notification_prefs(state: State<'_, AppState>) -> Result<NotificationPrefsView, CmdError> {
    ask(&state, |r| CommentCmd::Prefs(None, r))
}

/// Replaces the notification mutes; returns what was saved.
#[tauri::command(async)]
pub fn notification_prefs_set(
    state: State<'_, AppState>,
    prefs: NotificationPrefsView,
) -> Result<NotificationPrefsView, CmdError> {
    ask(&state, |r| CommentCmd::Prefs(Some(prefs), r))
}

/// The comment threads of a shared notebook as JSON, for an export with comments. `None` for a
/// notebook that isn't shared.
pub fn export_json(
    state: &State<'_, AppState>,
    notebook_id: Uuid,
) -> Result<Option<Vec<u8>>, CmdError> {
    if !state.sync.running() {
        return Ok(None);
    }
    ask(state, |r| CommentCmd::Export(notebook_id, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_serialize_as_the_contract_says() {
        let p = PlacementView::Orphaned {
            reason: "textChanged".into(),
            last_page_id: Some(Uuid::from_u128(1)),
            quote: Some("q".into()),
        };
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(j["kind"], "orphaned");
        assert_eq!(j["reason"], "textChanged");
        assert!(j.get("lastPageId").is_some());
        let placed = serde_json::to_value(PlacementView::Placed {
            page_id: Uuid::from_u128(2),
            rect: RectView {
                x: 1.0,
                y: 2.0,
                w: 3.0,
                h: 4.0,
            },
        })
        .unwrap();
        assert_eq!(placed["kind"], "placed");
        assert_eq!(placed["rect"]["w"], 3.0);
        assert_eq!(
            serde_json::to_value(DeliveryView::Waiting).unwrap(),
            "waiting"
        );
        let prefs: NotificationPrefsView = serde_json::from_value(serde_json::json!({
            "mutedNotebooks": [Uuid::from_u128(3)],
            "mutedThreads": [{"notebookId": Uuid::from_u128(3), "threadId": Uuid::from_u128(4)}]
        }))
        .unwrap();
        assert_eq!(prefs.muted_threads.len(), 1);
        assert_eq!(orphan_reason(OrphanReason::PageTrashed), "pageDeleted");
        assert_eq!(orphan_reason(OrphanReason::PdfPageMissing), "missing");
    }
}
