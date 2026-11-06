//! Touchpad pinch on Linux.
//!
//! WebKitGTK zooms the whole page on a touchpad pinch, through a GTK gesture on
//! its widget. The page never hears about it, so nothing in the webview can
//! cancel it, and the app's own zoom (which only moves the canvas) is left out.
//! Windows doesn't need this: wry switches WebView2's pinch zoom off. Nor does
//! macOS, where WKWebView only magnifies when asked to.
//!
//! GTK 3 has no public way to run ahead of a child widget's gestures, so this
//! wraps GDK's event handler, the step before GTK dispatches anything. Pinches
//! stop here and go to the editor as [`EVENT`]; every other event carries on
//! through `gtk_main_do_event`, which is what the default handler does. A pinch
//! in the library, which has nothing to zoom, is claimed the same way and does
//! nothing.

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use serde::Serialize;
use tauri::{Emitter, WebviewWindow};

/// Event carrying a [`Pinch`] to the webview.
pub const EVENT: &str = "penroam://pinch";

/// One step of a pinch.
#[derive(Clone, Serialize)]
struct Pinch {
    /// `begin`, `update` or `end`.
    phase: &'static str,
    /// Scale relative to the start of the gesture.
    scale: f64,
    /// Fingers' position in webview coordinates.
    x: f64,
    /// See `x`.
    y: f64,
}

/// Reads a touchpad pinch event, if `event` is one, into webview coordinates.
fn read(event: &mut gtk::gdk::Event, content: &gtk::Box) -> Option<Pinch> {
    if event.event_type() != gtk::gdk::EventType::TouchpadPinch {
        return None;
    }
    let pinch = event
        .clone()
        .downcast::<gtk::gdk::EventTouchpadPinch>()
        .ok()?;
    // SAFETY: `pinch` is a live event. The bindings don't expose the phase.
    let phase = match unsafe { (*pinch.to_glib_none().0).phase } as i32 {
        gtk::gdk::ffi::GDK_TOUCHPAD_GESTURE_PHASE_BEGIN => "begin",
        gtk::gdk::ffi::GDK_TOUCHPAD_GESTURE_PHASE_UPDATE => "update",
        _ => "end",
    };
    let (x, y) = pinch.position();
    // Events are relative to their GDK window, which can include a title bar.
    let (x, y) = gtk::event_widget(event)
        .and_then(|w| w.translate_coordinates(content, x as i32, y as i32))
        .map_or((x, y), |(x, y)| (x as f64, y as f64));
    Some(Pinch {
        phase,
        scale: pinch.scale(),
        x,
        y,
    })
}

/// Claims touchpad pinches for the editor. Call once, on the main thread.
pub fn install(window: &WebviewWindow) -> tauri::Result<()> {
    let content = window.default_vbox()?;
    let window = window.clone();
    gtk::gdk::Event::set_handler(Some(move |event: &mut gtk::gdk::Event| {
        match read(event, &content) {
            Some(pinch) => {
                let _ = window.emit(EVENT, pinch);
            }
            None => gtk::main_do_event(event),
        }
    }));
    Ok(())
}
