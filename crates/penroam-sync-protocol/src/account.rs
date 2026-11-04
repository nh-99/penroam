//! Account, plan and storage DTOs (capability `account.v1`, protocol 1.3).
//!
//! A hosted sync server can sell storage. This module is the provider-neutral part the public
//! app needs to show it: the account's plan and storage allowance, whether the account may
//! store new data, what the user can do next, and a server-made link that opens the server's
//! own billing pages in the system browser. Payment providers, prices and receipts stay inside
//! the server; the app never sees a provider id and never tells the server what was paid.
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
//!
//! Invariants that matter to clients:
//!
//! * Nothing here affects local notes. A read-only account still pulls, downloads blobs and
//!   exports; only uploads are refused (`quota_exceeded` on the write routes).
//! * Strings in [`plan_status`], [`read_only_reasons`] and [`actions`] are open sets: clients
//!   ignore values they do not know (an unknown action is not offered, an unknown status is
//!   shown by its plan name only).
//! * Store-distributed apps may be forbidden from showing [`AccountState::offers`] or opening a
//!   billing link. That decision belongs to the client build, not to the server.
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
}

impl PlanState {
    fn validate(&self) -> Result<(), &'static str> {
        if !label_ok(&self.id) || !label_ok(&self.name) || !label_ok(&self.status) {
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
}

impl PlanOffer {
    fn validate(&self) -> Result<(), &'static str> {
        if !label_ok(&self.id) || !label_ok(&self.name) || !label_ok(&self.price_label) {
            return Err("offer");
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
        }
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
