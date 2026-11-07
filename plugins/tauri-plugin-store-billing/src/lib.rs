//! In-app purchases through the app stores.
//!
//! This plugin only talks to the store. It decides nothing: what a purchase is worth comes from
//! the sync server, which looks the transaction up with the store (see `store-purchases.v1` in
//! `docs/protocol/sync-protocol-v1.md`).
//!
//! **A transaction must stay unfinished until the server has it.** The store redelivers an
//! unfinished transaction, and that is what makes a crash or an outage during a purchase safe.
//! So [`purchase()`] never finishes one; the caller does, with [`finish()`], once the server has
//! answered. The logic for that is `settle` in the app's `sync.rs`.
//!
//! The plugin has no commands, so the WebView can't reach it. iOS is implemented (StoreKit 2, in
//! Swift); on every other platform [`AVAILABLE`] is false and calls return an error.

#![warn(missing_docs)]

use serde::Deserialize;
#[cfg(target_os = "ios")]
use serde::{de::DeserializeOwned, Serialize};
#[cfg(target_os = "ios")]
use tauri::Manager;
use tauri::{
    plugin::{Builder, TauriPlugin},
    AppHandle, Runtime,
};

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_store_billing);

/// Whether this build can buy through a store.
pub const AVAILABLE: bool = cfg!(target_os = "ios");

/// A product the store can sell on this device.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreProduct {
    /// Store product id.
    pub id: String,
    /// The price, formatted by the store for the user's region. Never computed here: it's the
    /// amount the user will be charged.
    pub display_price: String,
    /// Billing period: `week`, `month` or `year`, when the store reports one.
    pub period: Option<String>,
}

/// How a purchase ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurchaseOutcome {
    /// Paid. Holds the transaction id, which is still unfinished.
    Purchased(String),
    /// Waiting for someone else, such as Ask to Buy. The transaction arrives later.
    Pending,
    /// The user closed the purchase sheet.
    Cancelled,
}

#[derive(Deserialize)]
struct PurchaseAnswer {
    status: String,
    transaction: Option<String>,
}

fn outcome(answer: PurchaseAnswer) -> Result<PurchaseOutcome, String> {
    match (answer.status.as_str(), answer.transaction) {
        ("purchased", Some(t)) => Ok(PurchaseOutcome::Purchased(t)),
        ("pending", _) => Ok(PurchaseOutcome::Pending),
        ("cancelled", _) => Ok(PurchaseOutcome::Cancelled),
        _ => Err("the store gave an unexpected answer".into()),
    }
}

#[derive(Deserialize)]
struct Transactions {
    transactions: Vec<String>,
}

#[derive(Deserialize)]
struct Products {
    products: Vec<StoreProduct>,
}

#[cfg(target_os = "ios")]
struct Native<R: Runtime>(tauri::plugin::PluginHandle<R>);

#[cfg(target_os = "ios")]
fn call<R: Runtime, T: DeserializeOwned>(
    app: &AppHandle<R>,
    method: &str,
    args: impl Serialize,
) -> Result<T, String> {
    app.state::<Native<R>>()
        .0
        .run_mobile_plugin(method, args)
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "ios"))]
fn call<R: Runtime, T>(
    _app: &AppHandle<R>,
    _method: &str,
    _args: serde_json::Value,
) -> Result<T, String> {
    Err("this build can't buy through a store".into())
}

/// The products the store can sell here, out of `ids`. An id the store doesn't know is left out,
/// so a misconfigured offer isn't shown.
///
/// # Errors
/// A readable message when the store can't be reached.
pub fn products<R: Runtime>(
    app: &AppHandle<R>,
    ids: &[String],
) -> Result<Vec<StoreProduct>, String> {
    let p: Products = call(app, "products", serde_json::json!({ "ids": ids }))?;
    Ok(p.products)
}

/// Shows the store's purchase sheet and blocks until it closes, so call it off the main thread
/// and without holding locks. `account_token` ties the purchase to the account.
///
/// # Errors
/// A readable message when the purchase failed. Nothing was charged in that case.
pub fn purchase<R: Runtime>(
    app: &AppHandle<R>,
    product_id: &str,
    account_token: &str,
) -> Result<PurchaseOutcome, String> {
    let answer: PurchaseAnswer = call(
        app,
        "purchase",
        serde_json::json!({ "productId": product_id, "accountToken": account_token }),
    )?;
    outcome(answer)
}

/// Transactions still waiting to be finished: purchases interrupted by a crash or an outage,
/// renewals, and approved Ask to Buy requests.
///
/// # Errors
/// A readable message when the store can't be reached.
pub fn unfinished<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<String>, String> {
    let t: Transactions = call(app, "unfinished", serde_json::json!({}))?;
    Ok(t.transactions)
}

/// Asks the store to bring this device up to date, which can prompt for the store account, and
/// returns the subscriptions this store account holds.
///
/// # Errors
/// A readable message when it failed or the user cancelled the store's sign-in.
pub fn restore<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<String>, String> {
    let t: Transactions = call(app, "restore", serde_json::json!({}))?;
    Ok(t.transactions)
}

/// Tells the store a transaction is dealt with, so it stops redelivering it. Only call this once
/// the server has answered: see the module docs.
///
/// # Errors
/// A readable message when the store can't be reached. The transaction is delivered again.
pub fn finish<R: Runtime>(app: &AppHandle<R>, transaction: &str) -> Result<(), String> {
    let _: serde_json::Value = call(
        app,
        "finish",
        serde_json::json!({ "transaction": transaction }),
    )?;
    Ok(())
}

/// The plugin (`store-billing`). Registers the native class on iOS and does nothing elsewhere.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("store-billing")
        .setup(|_app, _api| {
            #[cfg(target_os = "ios")]
            {
                let handle = _api.register_ios_plugin(init_plugin_store_billing)?;
                _app.manage(Native(handle));
            }
            Ok(())
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(status: &str, transaction: Option<&str>) -> PurchaseAnswer {
        PurchaseAnswer {
            status: status.into(),
            transaction: transaction.map(String::from),
        }
    }

    #[test]
    fn purchase_answers_are_read_strictly() {
        assert_eq!(
            outcome(answer("purchased", Some("7"))),
            Ok(PurchaseOutcome::Purchased("7".into()))
        );
        assert_eq!(
            outcome(answer("pending", None)),
            Ok(PurchaseOutcome::Pending)
        );
        assert_eq!(
            outcome(answer("cancelled", None)),
            Ok(PurchaseOutcome::Cancelled)
        );
        assert!(
            outcome(answer("purchased", None)).is_err(),
            "paid, but no id"
        );
        assert!(outcome(answer("later", None)).is_err());
    }
}
