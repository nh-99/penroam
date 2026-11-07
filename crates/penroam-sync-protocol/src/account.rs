//! Account, plan and storage DTOs (capability `account.v1`, protocol 1.3).
//!
//! A hosted sync server can sell storage. This module is the provider-neutral part the public
//! app needs to show it: the account's plan and storage allowance, whether the account may
//! store new data, what the user can do next, and a server-made link that opens the server's
//! own billing pages in the system browser. Payment providers, prices and receipts stay inside
//! the server; the app never sees a provider id and never tells the server what was paid.
//!
//! Plans bought through an app store (capability `store-purchases.v1`, protocol 1.5) work the
//! same way. The store takes the payment, and the app passes on the store's own identifier for
//! it. That identifier is only a lookup key: the server asks the store what it means and
//! derives the plan from the answer, so a client can't claim a payment by inventing one.
//!
//! Routes (all need an account or device session unless stated):
//!
//! * `GET /v1/account` returns an [`AccountState`].
//! * `POST /v1/account/billing-link` takes a [`BillingLinkRequest`] naming an action and, for
//!   [`actions::SUBSCRIBE`], one of the [`AccountState::offers`] ids. It returns a
//!   [`BillingLink`]: an `https://` URL on the server's billing pages, valid for a short time.
//!   The server decides which price an offer id means; a client cannot pick or invent one.
//! * `POST /v1/account/deletion` asks for the account to be erased after a cooling-off period
//!   and returns the [`DeletionState`]. It needs an **account session** obtained by a sign-in
//!   within the server's re-authentication window (otherwise `reauth_required`). `DELETE` on
//!   the same route withdraws the request while the cooling-off period runs.
//! * `POST /v1/account/store-purchase` takes a [`StorePurchaseRequest`] and returns the updated
//!   [`AccountState`]. `conflict` means the purchase belongs to another account. Retryable
//!   errors mean the server couldn't reach the store yet: the app keeps the store transaction
//!   unfinished and tries again, so a purchase is never lost.
//!
//! Invariants that matter to clients:
//!
//! * Nothing here affects local notes. A read-only account still pulls, downloads blobs and
//!   exports; only uploads are refused (`quota_exceeded` on the write routes).
//! * Strings in [`plan_status`], [`read_only_reasons`] and [`actions`] are open sets: clients
//!   ignore values they do not know (an unknown action is not offered, an unknown status is
//!   shown by its plan name only).
//! * Store-distributed apps may be forbidden from showing [`AccountState::offers`] or opening a
//!   billing link. That decision belongs to the client build, not to the server. Those builds
//!   sell [`PlanOffer::store_products`] instead, under [`actions::SUBSCRIBE_IN_STORE`].
//!
//! Normative description: `docs/protocol/sync-protocol-v1.md` §12b.

use serde::{Deserialize, Serialize};

use crate::ids::AccountId;

/// Longest accepted [`BillingLink::url`].
pub const MAX_BILLING_URL_LEN: usize = 2048;
/// Longest accepted id or label string in this module's DTOs.
pub const MAX_LABEL_LEN: usize = 128;
/// Most offers an [`AccountState`] may list.
pub const MAX_OFFERS: usize = 8;
/// Longest accepted [`StorePurchaseRequest::transaction`]. Play purchase tokens are long.
pub const MAX_STORE_TRANSACTION_LEN: usize = 4096;

/// App stores, in [`StorePurchaseRequest::store`] and [`PlanState::managed_in`].
pub mod stores {
    /// The Apple App Store.
    pub const APP_STORE: &str = "app_store";
    /// Google Play.
    pub const GOOGLE_PLAY: &str = "google_play";
}

/// Values of [`PlanState::managed_in`]: where the user changes or cancels a paid plan. An open
/// set, like the other strings here.
pub mod plan_managers {
    /// The server's billing pages ([`super::actions::MANAGE_BILLING`]).
    pub const WEB: &str = "web";
    /// App Store subscription settings.
    pub const APP_STORE: &str = super::stores::APP_STORE;
    /// Google Play subscription settings.
    pub const GOOGLE_PLAY: &str = super::stores::GOOGLE_PLAY;
}

/// Values of [`PlanState::status`].
pub mod plan_status {
    /// No paid plan: the free allowance applies.
    pub const FREE: &str = "free";
    /// Paid plan in a trial period.
    pub const TRIALING: &str = "trialing";
    /// Paid plan in good standing.
    pub const ACTIVE: &str = "active";
    /// Paid plan that ends at [`super::PlanState::ends_at_unix_s`] (cancelled for the period end).
    pub const CANCELING: &str = "canceling";
    /// A payment failed. The plan's allowance still applies until
    /// [`super::PlanState::ends_at_unix_s`] (the grace period); then the free allowance does.
    pub const PAST_DUE: &str = "past_due";
    /// Allowance granted by the operator (beta, support), not paid. May end at
    /// [`super::PlanState::ends_at_unix_s`].
    pub const GRANTED: &str = "granted";
}

/// Values of [`AccountState::read_only_reason`].
pub mod read_only_reasons {
    /// Stored data exceeds the current allowance (for example after a plan ended).
    pub const OVER_QUOTA: &str = "over_quota";
    /// The operator suspended writes (abuse, payment dispute under review).
    pub const SUSPENDED: &str = "suspended";
    /// The account is scheduled for deletion.
    pub const DELETION_PENDING: &str = "deletion_pending";
}

/// Values of [`AccountState::actions`] and [`BillingLinkRequest::action`].
pub mod actions {
    /// Start a paid plan ([`super::BillingLinkRequest`] with an offer id).
    pub const SUBSCRIBE: &str = "subscribe";
    /// Start a paid plan through an app store, using [`super::PlanOffer::store_products`] and
    /// [`super::AccountState::store_account_token`], then `POST /v1/account/store-purchase`.
    pub const SUBSCRIBE_IN_STORE: &str = "subscribe_in_store";
    /// Open the billing account: payment method, invoices, cancellation ([`super::BillingLinkRequest`]).
    pub const MANAGE_BILLING: &str = "manage_billing";
    /// `POST /v1/account/deletion`.
    pub const REQUEST_DELETION: &str = "request_deletion";
    /// `DELETE /v1/account/deletion`.
    pub const CANCEL_DELETION: &str = "cancel_deletion";
}

/// `GET /v1/account` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountState {
    /// The account.
    pub account_id: AccountId,
    /// Current plan.
    pub plan: PlanState,
    /// Storage use and allowance.
    pub storage: StorageState,
    /// Whether uploads are accepted right now.
    pub writable: bool,
    /// Why uploads are refused (one of [`read_only_reasons`]); absent when writable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_reason: Option<String>,
    /// Paid plans this account can start. Empty when the server sells nothing or the account
    /// already has a plan.
    #[serde(default)]
    pub offers: Vec<PlanOffer>,
    /// What the user can do now (values of [`actions`]).
    #[serde(default)]
    pub actions: Vec<String>,
    /// Present while a deletion request is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion: Option<DeletionState>,
    /// Opaque UUID to attach to store purchases (Apple's `appAccountToken`, Play's
    /// `obfuscatedAccountId`), so a purchase can be tied to this account. It isn't the account
    /// id, which stays out of the stores' records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_account_token: Option<String>,
}

impl AccountState {
    /// Whether `action` (one of [`actions`]) is offered.
    pub fn allows(&self, action: &str) -> bool {
        self.actions.iter().any(|a| a == action)
    }

    /// Check bounds. Clients call this before trusting a response.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.plan.validate()?;
        if self.offers.len() > MAX_OFFERS {
            return Err("too many offers");
        }
        for o in &self.offers {
            o.validate()?;
        }
        if self.actions.len() > 16 || self.actions.iter().any(|a| !label_ok(a)) {
            return Err("actions");
        }
        if self
            .read_only_reason
            .as_deref()
            .is_some_and(|r| !label_ok(r))
        {
            return Err("read_only_reason");
        }
        if self.writable == self.read_only_reason.is_some() {
            return Err("writable and read_only_reason disagree");
        }
        if self
            .store_account_token
            .as_deref()
            .is_some_and(|t| !is_uuid(t))
        {
            return Err("store_account_token");
        }
        Ok(())
    }
}

/// The account's plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanState {
    /// Server-defined plan id (`free` for no plan).
    pub id: String,
    /// Display name.
    pub name: String,
    /// One of [`plan_status`].
    pub status: String,
    /// Next renewal (Unix seconds), for paid plans that renew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renews_at_unix_s: Option<u64>,
    /// When the plan's allowance ends (Unix seconds): period end of a cancelled plan, end of the
    /// payment grace period, end of a grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at_unix_s: Option<u64>,
    /// Where a paid plan is managed (one of [`plan_managers`]). Absent when there's nothing to
    /// manage. A store plan can't be changed or cancelled through this server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_in: Option<String>,
}

impl PlanState {
    fn validate(&self) -> Result<(), &'static str> {
        if !label_ok(&self.id) || !label_ok(&self.name) || !label_ok(&self.status) {
            return Err("plan");
        }
        if self.managed_in.as_deref().is_some_and(|m| !label_ok(m)) {
            return Err("plan");
        }
        Ok(())
    }
}

/// Storage counters of the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageState {
    /// Committed ciphertext bytes (operations, blobs, checkpoints).
    pub used_bytes: u64,
    /// Current allowance in bytes (`None` = unlimited).
    pub quota_bytes: Option<u64>,
    /// Allowance every account keeps without a plan (`None` = the server has no free
    /// allowance, or it is unlimited when `quota_bytes` is too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_quota_bytes: Option<u64>,
}

/// A paid plan the account can start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOffer {
    /// Server-defined offer id to pass in [`BillingLinkRequest::offer_id`].
    pub id: String,
    /// Display name.
    pub name: String,
    /// Display price including currency and interval, formatted by the server
    /// (e.g. `$3.99 / month`). The billing page shows the binding price.
    pub price_label: String,
    /// Allowance the plan gives.
    pub quota_bytes: u64,
    /// The same plan's products in the app stores, if it's sold there. A store app shows the
    /// price the store reports, not [`Self::price_label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_products: Option<StoreProducts>,
}

impl PlanOffer {
    fn validate(&self) -> Result<(), &'static str> {
        if !label_ok(&self.id) || !label_ok(&self.name) || !label_ok(&self.price_label) {
            return Err("offer");
        }
        if let Some(p) = &self.store_products {
            let ids = [
                &p.apple_product_id,
                &p.google_product_id,
                &p.google_base_plan_id,
            ];
            let bad = |id: &&Option<String>| id.as_ref().is_some_and(|id| !label_ok(id));
            if ids.iter().any(bad) {
                return Err("offer");
            }
        }
        Ok(())
    }
}

/// Store product identifiers of a [`PlanOffer`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreProducts {
    /// App Store product id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apple_product_id: Option<String>,
    /// Google Play subscription product id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_product_id: Option<String>,
    /// Google Play base plan of that subscription (monthly and yearly are two base plans).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google_base_plan_id: Option<String>,
}

/// `POST /v1/account/store-purchase` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorePurchaseRequest {
    /// One of [`stores`].
    pub store: String,
    /// The store's identifier for the purchase: an App Store transaction id, or a Play purchase
    /// token. The server looks it up with the store and trusts nothing else about it.
    pub transaction: String,
}

impl StorePurchaseRequest {
    /// Check bounds. The server calls this before asking a store.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !label_ok(&self.store) {
            return Err("store");
        }
        let t = &self.transaction;
        if t.is_empty()
            || t.len() > MAX_STORE_TRANSACTION_LEN
            || t.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err("transaction");
        }
        Ok(())
    }
}

/// `POST /v1/account/billing-link` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingLinkRequest {
    /// [`actions::SUBSCRIBE`] or [`actions::MANAGE_BILLING`].
    pub action: String,
    /// For [`actions::SUBSCRIBE`]: an id from [`AccountState::offers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_id: Option<String>,
}

/// `POST /v1/account/billing-link` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingLink {
    /// Page to open in the system browser (never in an embedded view).
    pub url: String,
    /// Seconds the link stays valid.
    pub expires_in_s: u64,
}

impl BillingLink {
    /// Accepts only an absolute `https://` URL without credentials or spaces, of bounded
    /// length. Clients refuse to open anything else.
    pub fn validate(&self) -> Result<(), &'static str> {
        let u = &self.url;
        if u.len() > MAX_BILLING_URL_LEN || u.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("billing link is too long or has spaces");
        }
        let Some(rest) = u.strip_prefix("https://") else {
            return Err("billing link must be https");
        };
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        if host.is_empty() || host.contains('@') {
            return Err("billing link has no host or carries credentials");
        }
        Ok(())
    }
}

/// A pending deletion request (`POST /v1/account/deletion` response and
/// [`AccountState::deletion`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionState {
    /// When the request was made (Unix seconds).
    pub requested_at_unix_s: u64,
    /// Earliest time the server erases the account (Unix seconds). Until then the request can
    /// be withdrawn and data can still be downloaded.
    pub erase_after_unix_s: u64,
}

fn label_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_LABEL_LEN && !s.chars().any(char::is_control)
}

/// The hyphenated, lowercase form both stores accept.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AccountState {
        AccountState {
            account_id: "acct-1".parse().unwrap(),
            plan: PlanState {
                id: "free".into(),
                name: "Free".into(),
                status: plan_status::FREE.into(),
                renews_at_unix_s: None,
                ends_at_unix_s: None,
                managed_in: None,
            },
            storage: StorageState {
                used_bytes: 10,
                quota_bytes: Some(100),
                free_quota_bytes: Some(100),
            },
            writable: true,
            read_only_reason: None,
            offers: vec![],
            actions: vec![actions::REQUEST_DELETION.into()],
            deletion: None,
            store_account_token: None,
        }
    }

    #[test]
    fn store_account_tokens_are_uuids() {
        let mut s = state();
        s.store_account_token = Some("3f2c1b7e-8d4a-4f6e-9b21-0c7d5e1a2b3c".into());
        s.validate().unwrap();
        for bad in ["acct-1", "", "3F2C1B7E-8D4A-4F6E-9B21-0C7D5E1A2B3C"] {
            s.store_account_token = Some(bad.into());
            assert!(s.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn store_purchase_requests_are_bounded() {
        let req = |t: &str| StorePurchaseRequest {
            store: stores::APP_STORE.into(),
            transaction: t.into(),
        };
        req("2000000123456789").validate().unwrap();
        req(&"a".repeat(MAX_STORE_TRANSACTION_LEN))
            .validate()
            .unwrap();
        for bad in [
            String::new(),
            "a b".to_string(),
            "a\nb".to_string(),
            "a".repeat(MAX_STORE_TRANSACTION_LEN + 1),
        ] {
            assert!(req(&bad).validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn older_servers_and_clients_stay_compatible() {
        // A 1.4 server sends none of the store fields.
        let old = r#"{"account_id":"acct-1","plan":{"id":"free","name":"Free","status":"free"},
            "storage":{"used_bytes":0,"quota_bytes":100},"writable":true,
            "offers":[{"id":"m","name":"M","price_label":"$1","quota_bytes":5}]}"#;
        let s: AccountState = serde_json::from_str(old).unwrap();
        s.validate().unwrap();
        assert!(s.store_account_token.is_none() && s.offers[0].store_products.is_none());
        // Without store fields, nothing new is written for a 1.4 client to meet.
        let out = serde_json::to_string(&state()).unwrap();
        assert!(!out.contains("store") && !out.contains("managed_in"));
    }

    #[test]
    fn state_validation() {
        let s = state();
        s.validate().unwrap();
        assert!(s.allows(actions::REQUEST_DELETION));
        assert!(!s.allows(actions::SUBSCRIBE));
        let mut bad = s.clone();
        bad.writable = false;
        assert!(bad.validate().is_err(), "read-only needs a reason");
        bad.read_only_reason = Some(read_only_reasons::OVER_QUOTA.into());
        bad.validate().unwrap();
        let mut bad = s.clone();
        bad.plan.name = "a\nb".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn unknown_fields_and_values_are_tolerated() {
        let j = r#"{"account_id":"acct-1","plan":{"id":"x","name":"X","status":"future"},
            "storage":{"used_bytes":0,"quota_bytes":null},"writable":true,
            "actions":["teleport"],"something_new":1}"#;
        let s: AccountState = serde_json::from_str(j).unwrap();
        s.validate().unwrap();
        assert!(!s.allows(actions::MANAGE_BILLING));
    }

    #[test]
    fn billing_links() {
        let ok = |u: &str| {
            BillingLink {
                url: u.into(),
                expires_in_s: 60,
            }
            .validate()
        };
        ok("https://billing.example/c/pay/cs_test_1#frag").unwrap();
        for bad in [
            "http://billing.example/x",
            "https://",
            "https://user:pw@billing.example/",
            "https://billing.example/a b",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
        assert!(ok(&format!("https://e.example/{}", "a".repeat(3000))).is_err());
    }
}
