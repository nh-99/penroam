//! The Mac App Store build's way into StoreKit.
//!
//! Tauri's mobile plugin bridge doesn't exist on macOS, so the Swift side (`macos/Sources`) exports
//! one C function that takes a method name and JSON arguments and answers with a JSON envelope,
//! `{"ok": …}` or `{"error": "…"}`. The methods and their answers are the same as the iOS plugin's,
//! so the rest of this crate doesn't know which one it is talking to.
//!
//! Each call blocks until StoreKit answers. Callers are already off the main thread (see the
//! crate docs), and the main thread must stay free to show the purchase sheet.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::ffi::{c_char, c_void, CStr, CString};

extern "C" {
    fn penroam_store_billing_start();
    fn penroam_store_billing_call(method: *const c_char, args: *const c_char) -> *mut c_char;
    fn free(p: *mut c_void);
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Envelope<T> {
    Ok { ok: T },
    Err { error: String },
}

/// Starts StoreKit's transaction listener. Call once, at launch.
pub(crate) fn start() {
    // SAFETY: takes no arguments, and the Swift side makes repeated calls harmless.
    unsafe { penroam_store_billing_start() }
}

/// Runs one store method.
pub(crate) fn call<T: DeserializeOwned>(method: &str, args: impl Serialize) -> Result<T, String> {
    let method = CString::new(method).map_err(|e| e.to_string())?;
    let args = serde_json::to_string(&args).map_err(|e| e.to_string())?;
    let args = CString::new(args).map_err(|e| e.to_string())?;
    // SAFETY: both pointers are valid NUL-terminated strings for the duration of the call, and
    // the Swift side returns a `strdup`ed string (or null) that we own and free exactly once.
    let text = unsafe {
        let p = penroam_store_billing_call(method.as_ptr(), args.as_ptr());
        if p.is_null() {
            return Err("the App Store gave no answer".into());
        }
        let text = CStr::from_ptr(p).to_string_lossy().into_owned();
        free(p.cast());
        text
    };
    read(&text)
}

fn read<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    match serde_json::from_str::<Envelope<T>>(text) {
        Ok(Envelope::Ok { ok }) => Ok(ok),
        Ok(Envelope::Err { error }) => Err(error),
        Err(_) => Err("the App Store gave an unexpected answer".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_and_errors_are_told_apart() {
        assert_eq!(
            read::<Vec<String>>(r#"{"ok":["7"]}"#),
            Ok(vec!["7".to_string()])
        );
        assert_eq!(
            read::<Vec<String>>(r#"{"error":"offline"}"#),
            Err("offline".into())
        );
        assert!(read::<Vec<String>>("not json").is_err());
    }
}
