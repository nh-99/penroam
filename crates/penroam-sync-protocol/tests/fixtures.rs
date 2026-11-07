//! Public schema compatibility check.
//!
//! Every committed fixture in `fixtures/v1/` must deserialize into the
//! current types and re-serialize **byte-identically**; binary and hex
//! fixtures must be reproduced exactly by the current encoders. A failure
//! here means a wire-visible change: either it is a bug, or it needs a
//! protocol/envelope version decision and regenerated fixtures
//! (`cargo run -p penroam-sync-testkit --bin penroam-sync-fixtures -- crates/penroam-sync-protocol/fixtures/v1`).

use std::collections::BTreeSet;
use std::path::PathBuf;

use penroam_sync_protocol::contract::{
    error_code_table, to_fixture_json, ErrorCodeEntry, InvalidEnvelopeCase,
};
use penroam_sync_protocol::framing::{
    decode_batch, decode_json_batch, encode_batch, encode_json_batch,
};
use penroam_sync_protocol::{
    canonical_aad_bytes, canonical_signing_bytes, hex, negotiate, verify_ciphertext, AckRequest,
    ContractManifest, DeviceRegistrationRequest, DeviceRegistrationResponse, EnvelopeHeader,
    ErrorBody, Limits, ProtocolRange, PullOpsRequest, PullOpsResponse, PulledOp, PushOpsResponse,
    ServerMeta, UsageResponse,
};
use penroam_sync_protocol::{
    canonical_device_auth_bytes, AuthConfig, DeviceChallenge, DeviceChallengeRequest,
    DeviceSessionRequest, RefreshRequest, SessionRequest, SessionTokens,
};
use serde::{de::DeserializeOwned, Serialize};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/v1")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn roundtrip<T: Serialize + DeserializeOwned>(name: &str) -> T {
    let bytes = read(name);
    let text = std::str::from_utf8(&bytes).unwrap();
    let value: T = serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(
        to_fixture_json(&value),
        text,
        "{name} does not re-serialize byte-identically"
    );
    value
}

#[test]
fn every_fixture_is_checked() {
    let mut checked: BTreeSet<String> = [
        "meta.json",
        "contract-manifest.json",
        "error-codes.json",
        "envelope-header.valid.json",
        "envelope-header.valid.signing-bytes.hex",
        "envelope-header.valid.aad-bytes.hex",
        "push-batch.bin",
        "push-batch.retry.bin",
        "push-response.accepted.json",
        "push-response.retry.json",
        "pull-request.page1.json",
        "pull-request.page2.json",
        "pull-response.page1.json",
        "pull-response.page2.json",
        "pull-batch.page1.bin",
        "pull-batch.page2.bin",
        "ack-request.json",
        "usage-response.json",
        "device-registration-request.json",
        "device-registration-response.json",
        "error.cursor-expired.json",
        "error.rate-limited.json",
        "error.upgrade-required.json",
        "auth-config.json",
        "auth-config.loopback-ports.json",
        "auth-session-request.json",
        "auth-refresh-request.json",
        "auth-session-tokens.account.json",
        "auth-session-tokens.device.json",
        "auth-device-challenge-request.json",
        "auth-device-challenge.json",
        "auth-device-session-request.json",
        "auth-device-session.signing-bytes.hex",
        "account-state.free.json",
        "account-state.over-quota.json",
        "account-state.past-due.json",
        "account-state.store-offers.json",
        "account-store-purchase-request.json",
        "account-billing-link-request.json",
        "account-billing-link.json",
        "account-deletion.json",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let mut on_disk = BTreeSet::new();
    for e in std::fs::read_dir(dir()).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        if name.starts_with("envelope-header.invalid.") {
            checked.insert(name.clone());
        }
        on_disk.insert(name);
    }
    assert_eq!(on_disk, checked, "fixture set changed; update this test");
}

#[test]
fn meta_and_manifest() {
    let meta: ServerMeta = roundtrip("meta.json");
    meta.limits.validate().unwrap();
    assert!(negotiate(ProtocolRange::CLIENT, &meta).is_ok());
    let manifest: ContractManifest = roundtrip("contract-manifest.json");
    assert_eq!(
        manifest,
        ContractManifest::current(),
        "contract numbers changed"
    );
    let codes: Vec<ErrorCodeEntry> = roundtrip("error-codes.json");
    assert_eq!(codes, error_code_table(), "error code table changed");
}

#[test]
fn valid_envelope_and_canonical_bytes() {
    let h: EnvelopeHeader = roundtrip("envelope-header.valid.json");
    h.validate(&Limits::DEFAULT).unwrap();
    let sig = String::from_utf8(read("envelope-header.valid.signing-bytes.hex")).unwrap();
    assert_eq!(sig.trim_end(), hex::encode(&canonical_signing_bytes(&h)));
    let aad = String::from_utf8(read("envelope-header.valid.aad-bytes.hex")).unwrap();
    assert_eq!(aad.trim_end(), hex::encode(&canonical_aad_bytes(&h)));
    // depends_on order on the wire must not matter.
    let mut rev = h.clone();
    rev.depends_on.reverse();
    assert_eq!(sig.trim_end(), hex::encode(&canonical_signing_bytes(&rev)));
}

#[test]
fn invalid_envelopes_fail_with_expected_error() {
    let mut n = 0;
    for e in std::fs::read_dir(dir()).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        if !name.starts_with("envelope-header.invalid.") {
            continue;
        }
        let case: InvalidEnvelopeCase = roundtrip(&name);
        let err = case.header.validate(&Limits::DEFAULT).unwrap_err();
        assert_eq!(err.kind(), case.expected_error, "{name}");
        assert_eq!(err.error_code(), case.expected_error_code, "{name}");
        n += 1;
    }
    assert!(
        n >= 5,
        "expected several invalid envelope fixtures, found {n}"
    );
}

#[test]
fn push_batches_and_responses() {
    let l = Limits::DEFAULT;
    for name in ["push-batch.bin", "push-batch.retry.bin"] {
        let bytes = read(name);
        let raw = decode_batch(&bytes, &l).unwrap();
        assert_eq!(encode_batch(&raw, &l).unwrap(), bytes, "{name}");
        let typed: Vec<(EnvelopeHeader, Vec<u8>)> = decode_json_batch(&bytes, &l).unwrap();
        assert_eq!(
            encode_json_batch(&typed, &l).unwrap(),
            bytes,
            "{name} typed"
        );
        if name == "push-batch.bin" {
            for (h, ct) in &typed {
                h.validate(&l).unwrap();
                verify_ciphertext(h, ct).unwrap();
            }
        }
    }
    let _: PushOpsResponse = roundtrip("push-response.accepted.json");
    let _: PushOpsResponse = roundtrip("push-response.retry.json");
}

#[test]
fn pull_pages() {
    let l = Limits::DEFAULT;
    for page in ["page1", "page2"] {
        let _: PullOpsRequest = roundtrip(&format!("pull-request.{page}.json"));
        let resp: PullOpsResponse = roundtrip(&format!("pull-response.{page}.json"));
        let bytes = read(&format!("pull-batch.{page}.bin"));
        let items: Vec<(PulledOp, Vec<u8>)> = decode_json_batch(&bytes, &l).unwrap();
        assert_eq!(encode_json_batch(&items, &l).unwrap(), bytes);
        assert_eq!(
            items.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            resp.items
        );
        for (p, ct) in &items {
            verify_ciphertext(&p.header, ct).unwrap();
        }
        assert!(resp
            .items
            .windows(2)
            .all(|w| w[0].server_seq < w[1].server_seq));
    }
}

#[test]
fn misc_dtos_and_errors() {
    let _: AckRequest = roundtrip("ack-request.json");
    let _: UsageResponse = roundtrip("usage-response.json");
    let reg: DeviceRegistrationRequest = roundtrip("device-registration-request.json");
    reg.validate().unwrap();
    let _: DeviceRegistrationResponse = roundtrip("device-registration-response.json");
    for name in [
        "error.cursor-expired.json",
        "error.rate-limited.json",
        "error.upgrade-required.json",
    ] {
        let _: ErrorBody = roundtrip(name);
    }
}

#[test]
fn auth_documents() {
    let cfg: AuthConfig = roundtrip("auth-config.json");
    assert!(cfg.scopes.iter().any(|s| s == "openid"));
    assert_eq!(cfg.redirect, penroam_sync_protocol::auth::REDIRECT_LOOPBACK);
    cfg.validate().unwrap();
    let fixed: AuthConfig = roundtrip("auth-config.loopback-ports.json");
    fixed.validate().unwrap();
    assert_eq!(
        fixed.redirect,
        penroam_sync_protocol::auth::REDIRECT_LOOPBACK_PORTS
    );
    assert_eq!(fixed.loopback_ports, [47011, 47012, 47013]);
    let _: SessionRequest = roundtrip("auth-session-request.json");
    let _: RefreshRequest = roundtrip("auth-refresh-request.json");
    let acct: SessionTokens = roundtrip("auth-session-tokens.account.json");
    assert!(acct.device_id.is_none());
    let dev: SessionTokens = roundtrip("auth-session-tokens.device.json");
    let _: DeviceChallengeRequest = roundtrip("auth-device-challenge-request.json");
    let ch: DeviceChallenge = roundtrip("auth-device-challenge.json");
    let req: DeviceSessionRequest = roundtrip("auth-device-session-request.json");
    req.validate().unwrap();
    assert_eq!(req.challenge, ch.challenge);
    let meta: ServerMeta = roundtrip("meta.json");
    let bytes = String::from_utf8(read("auth-device-session.signing-bytes.hex")).unwrap();
    assert_eq!(
        bytes.trim_end(),
        hex::encode(&canonical_device_auth_bytes(
            &dev.account_id,
            dev.device_id.as_ref().unwrap(),
            &req.challenge,
            &meta.server_instance_id,
        ))
    );
}

#[test]
fn account_documents() {
    use penroam_sync_protocol::account::actions;
    use penroam_sync_protocol::{AccountState, BillingLink, BillingLinkRequest, DeletionState};
    for name in [
        "account-state.free.json",
        "account-state.over-quota.json",
        "account-state.past-due.json",
        "account-state.store-offers.json",
    ] {
        let s: AccountState = roundtrip(name);
        s.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let free: AccountState = roundtrip("account-state.free.json");
    assert!(free.allows(actions::SUBSCRIBE));
    let store: AccountState = roundtrip("account-state.store-offers.json");
    assert!(store.allows(penroam_sync_protocol::account::actions::SUBSCRIBE_IN_STORE));
    assert!(store.store_account_token.is_some());
    assert_eq!(store.offers.len(), 2);
    let purchase: penroam_sync_protocol::StorePurchaseRequest =
        roundtrip("account-store-purchase-request.json");
    purchase.validate().unwrap();
    let req: BillingLinkRequest = roundtrip("account-billing-link-request.json");
    assert_eq!(req.offer_id.as_deref(), Some(free.offers[0].id.as_str()));
    let link: BillingLink = roundtrip("account-billing-link.json");
    link.validate().unwrap();
    let _: DeletionState = roundtrip("account-deletion.json");
}

/// `docs/protocol/openapi.yaml` is hand-written; it must name every route.
#[test]
fn openapi_lists_every_route() {
    let spec = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol/openapi.yaml");
    let Ok(text) = std::fs::read_to_string(&spec) else {
        // the crate was packaged without the repository's docs
        return;
    };
    for route in penroam_sync_protocol::routes::ALL {
        assert!(
            text.contains(&format!("\n  {route}:\n")),
            "{route} missing from openapi.yaml"
        );
    }
}
