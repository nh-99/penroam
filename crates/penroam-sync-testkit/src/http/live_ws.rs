//! `GET /v1/live`: the WebSocket side of the live channel in the HTTP test server.
//!
//! [`upgrade`] runs on an HTTP worker: it checks the upgrade headers, the browser `Origin` (when
//! present it must be allow-listed; see [`InMemoryServer::set_live_allowed_origins`]), the bearer
//! token (a device session on a space) and the membership, then answers `101 Switching Protocols`.
//! Anything wrong is an ordinary HTTP error instead, so no socket is opened for a non-member.
//!
//! [`spawn`] then serves the socket on its own thread, so a long-lived connection never holds an
//! HTTP worker. The loop reads client text frames into the hub ([`InMemoryServer::live_send_text`])
//! and writes whatever the hub queued ([`InMemoryServer::live_recv`]), with a short read timeout so
//! both directions stay responsive. tungstenite enforces `max_frame_bytes` on incoming messages;
//! oversized, binary or malformed frames end the session with `bye invalid_frame`.

use std::io::{ErrorKind, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use penroam_sync_protocol::live::{Bye, ByeCode, ClientFrame, ServerFrame};
use penroam_sync_protocol::{ErrorCode, LiveSessionId};
use tungstenite::protocol::{Role, WebSocketConfig};
use tungstenite::{Message, WebSocket};

use super::{http_err, Ctx, Principal, Request, Response};
use crate::{Caller, LiveRefused};

/// Read timeout of the socket loop: how long a read waits before the loop flushes queued frames.
const POLL: Duration = Duration::from_millis(20);

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers.get(name).map(String::as_str)
}

/// Validates a `/v1/live` upgrade and writes the `101` answer. Returns the authenticated
/// principal, or the HTTP error to send instead.
pub(super) fn upgrade(
    ctx: &Arc<Ctx>,
    stream: &TcpStream,
    req: &Request,
    rid: &str,
) -> Result<Principal, Box<Response>> {
    let upgrade_ok = header(req, "upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
        && header(req, "connection").is_some_and(|c| {
            c.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        })
        && header(req, "sec-websocket-version") == Some("13");
    let Some(key) = header(req, "sec-websocket-key").filter(|_| upgrade_ok) else {
        return Err(http_err(
            ErrorCode::MalformedRequest,
            "/v1/live needs a WebSocket upgrade",
            rid,
        ));
    };
    if let Some(origin) = header(req, "origin") {
        if !ctx.server.live_origin_allowed(origin) {
            return Err(http_err(ErrorCode::Forbidden, "origin not allowed", rid));
        }
    }
    let token = header(req, "authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::trim);
    let principal = token
        .and_then(|t| {
            ctx.tokens
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(t)
                .cloned()
        })
        .ok_or_else(|| {
            http_err(
                ErrorCode::Unauthenticated,
                "missing or unknown bearer token",
                rid,
            )
        })?;
    let Some(device) = principal.device.clone() else {
        return Err(http_err(
            ErrorCode::Forbidden,
            "the live channel needs a device session",
            rid,
        ));
    };
    let caller = Caller {
        account: &principal.account,
        member: principal.member.as_ref(),
    };
    if let Err(mut body) = ctx.server.live_authorize(caller, &device) {
        body.request_id = rid.to_string();
        return Err(Box::new(Response::error(body)));
    }
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\nPenroam-Request-Id: {rid}\r\n\r\n"
    );
    let mut w = stream;
    w.write_all(head.as_bytes())
        .and_then(|_| w.flush())
        .map_err(|_| http_err(ErrorCode::InternalError, "could not upgrade", rid))?;
    Ok(principal)
}

/// Serves an upgraded socket on its own thread.
pub(super) fn spawn(ctx: Arc<Ctx>, stream: TcpStream, rest: Vec<u8>, principal: Principal) {
    std::thread::spawn(move || serve(&ctx, stream, rest, &principal));
}

/// The bye for a hello refused at the HTTP level after the upgrade (membership changed between
/// the upgrade and the hello).
fn bye_for(code: ErrorCode) -> Bye {
    let code = match code {
        ErrorCode::AccessRevoked | ErrorCode::NotFound | ErrorCode::DeviceRevoked => {
            ByeCode::AccessRevoked
        }
        ErrorCode::RateLimited => ByeCode::RateLimited,
        _ => ByeCode::Unauthenticated,
    };
    Bye {
        code,
        retry_after_ms: (code == ByeCode::RateLimited).then_some(5_000),
    }
}

fn serve(ctx: &Arc<Ctx>, stream: TcpStream, rest: Vec<u8>, principal: &Principal) {
    let server = &ctx.server;
    let cfg = server.live_config();
    let max = cfg.max_frame_bytes as usize;
    let config = WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max));
    let _ = stream.set_read_timeout(Some(POLL));
    let mut ws = WebSocket::from_partially_read(stream, rest, Role::Server, Some(config));
    let device = principal.device.clone().expect("checked at upgrade");
    let caller = Caller {
        account: &principal.account,
        member: principal.member.as_ref(),
    };
    let started = Instant::now();
    let hello_deadline = Duration::from_millis(u64::from(cfg.heartbeat_ms) * 2);
    let mut session: Option<LiveSessionId> = None;
    let send = |ws: &mut WebSocket<TcpStream>, f: &ServerFrame| {
        ws.send(Message::text(f.to_text())).is_ok()
    };
    let mut ending: Option<Bye> = None;
    loop {
        if ctx.stop.load(Ordering::SeqCst) {
            ending = Some(Bye {
                code: ByeCode::ServerRestart,
                retry_after_ms: Some(1_000),
            });
        }
        if ending.is_none() {
            match ws.read() {
                Ok(Message::Text(t)) => match &session {
                    Some(id) => server.live_send_text(id, t.as_str()),
                    None => match ClientFrame::parse(t.as_str(), cfg.max_frame_bytes) {
                        Ok(ClientFrame::Hello(h)) => match server.live_connect(caller, &device, &h)
                        {
                            Ok(w) => {
                                session = Some(w.session_id.clone());
                                if !send(&mut ws, &ServerFrame::Welcome(w)) {
                                    break;
                                }
                            }
                            Err(LiveRefused::Bye(b)) => ending = Some(b),
                            Err(LiveRefused::Http(body)) => ending = Some(bye_for(body.code)),
                        },
                        _ => {
                            ending = Some(Bye {
                                code: ByeCode::InvalidFrame,
                                retry_after_ms: None,
                            })
                        }
                    },
                },
                Ok(Message::Binary(_)) => {
                    ending = Some(Bye {
                        code: ByeCode::InvalidFrame,
                        retry_after_ms: None,
                    })
                }
                Ok(Message::Close(_)) => break,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(tungstenite::Error::Capacity(_)) => {
                    ending = Some(Bye {
                        code: ByeCode::InvalidFrame,
                        retry_after_ms: None,
                    })
                }
                Err(_) => break,
            }
        }
        if session.is_none() && ending.is_none() && started.elapsed() > hello_deadline {
            ending = Some(Bye {
                code: ByeCode::IdleTimeout,
                retry_after_ms: None,
            });
        }
        if let Some(id) = &session {
            server.live_tick();
            for f in server.live_recv(id) {
                let is_bye = matches!(f, ServerFrame::Bye(_));
                if !send(&mut ws, &f) {
                    ending = None;
                    session = None;
                    break;
                }
                if is_bye {
                    // the hub already sent its bye (and forgot the session); just close
                    close(&mut ws);
                    return;
                }
            }
            if session.is_none() {
                break;
            }
        }
        if let Some(bye) = ending.take() {
            if let Some(id) = &session {
                server.live_disconnect(id);
            }
            let _ = send(&mut ws, &ServerFrame::Bye(bye));
            close(&mut ws);
            return;
        }
        let _ = ws.flush();
    }
    if let Some(id) = &session {
        server.live_disconnect(id);
    }
}

/// Starts the closing handshake and waits briefly for the client's answer.
fn close(ws: &mut WebSocket<TcpStream>) {
    let _ = ws.close(None);
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        match ws.read() {
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                let _ = ws.flush();
            }
            Err(_) => break,
        }
    }
}
