//! Deterministic contract fixture generator.
//!
//! Everything here uses fixed ids, fixed "ciphertext" bytes and a fixed
//! server secret, so [`generate`] is byte-identical across runs and
//! platforms. Signatures are a **placeholder** byte pattern: these fixtures
//! pin wire shapes, not cryptography. Pinned test-key crypto vectors live in
//! `crates/penroam-sync/tests/vectors/crypto-vectors.json`.

use std::collections::BTreeMap;
use std::path::Path;

use penroam_sync_protocol::auth::{REDIRECT_LOOPBACK, REDIRECT_LOOPBACK_PORTS};
use penroam_sync_protocol::base64url;
use penroam_sync_protocol::contract::{error_code_table, to_fixture_json, InvalidEnvelopeCase};
use penroam_sync_protocol::envelope::sha256_hex;
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::hex;
use penroam_sync_protocol::{
    canonical_aad_bytes, canonical_device_auth_bytes, canonical_signing_bytes, AccountId,
    AckRequest, AuthConfig, ContractManifest, DeviceChallenge, DeviceChallengeRequest, DeviceId,
    DeviceRegistrationRequest, DeviceRegistrationResponse, DeviceSessionRequest, EnvelopeHeader,
    ErrorBody, ErrorCode, Limits, NotebookRoutingId, OpRef, PullOpsRequest, RefreshRequest,
    SessionRequest, SessionTokens,
};

use crate::{InMemoryServer, RequestError};

/// Fixed server secret used for fixtures (public, test-only).
pub const FIXTURE_SECRET: [u8; 32] = [0x5e; 32];
/// Fixed server instance id used for fixtures.
pub const FIXTURE_INSTANCE_ID: &str = "testkit-fixture-instance";

fn id<T: std::str::FromStr>(s: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    s.parse().expect("fixture ids are valid")
}

/// Deterministic pseudo-ciphertext for `label` (NOT real ciphertext):
/// concatenated `SHA-256(label || counter_le32)` blocks.
pub fn fake_ciphertext(label: &str, len: usize) -> Vec<u8> {
    use sha2::Digest;
    let mut out = Vec::with_capacity(len + 32);
    let mut counter = 0u32;
    while out.len() < len {
        out.extend_from_slice(
            &sha2::Sha256::new()
                .chain_update(label.as_bytes())
                .chain_update(counter.to_le_bytes())
                .finalize(),
        );
        counter += 1;
    }
    out.truncate(len);
    out
}

/// Placeholder signature: 64 bytes of `0xA5`, base64url. Not a signature.
pub fn placeholder_signature() -> String {
    base64url::encode(&[0xA5; 64])
}

/// A header for `ciphertext` with the given identity and dependencies.
pub fn header_for(
    op_id: &str,
    device: &str,
    seq: u64,
    deps: &[(&str, u64)],
    ciphertext: &[u8],
) -> EnvelopeHeader {
    EnvelopeHeader {
        envelope_version: 1,
        op_id: id(op_id),
        device_id: id(device),
        device_seq: seq,
        notebook_id: id::<NotebookRoutingId>("nb-7f3c2a9e"),
        depends_on: deps
            .iter()
            .map(|(d, s)| OpRef {
                device_id: id(d),
                device_seq: *s,
            })
            .collect(),
        key_epoch: 1,
        op_schema_version: 1,
        ciphertext_sha256: sha256_hex(ciphertext),
        ciphertext_length: ciphertext.len() as u64,
        signature: placeholder_signature(),
    }
}

const OP_A: &str = "0190c6f2-7b1a-7cc0-9d1e-000000000001";
const OP_B: &str = "0190c6f2-7b1a-7cc0-9d1e-000000000002";
const OP_C: &str = "0190c6f2-7b1a-7cc0-9d1e-000000000003";
const DEV_A: &str = "dev-a-3b1f";
const DEV_B: &str = "dev-b-c94e";
const ACCOUNT: &str = "acct-fixture-0001";

fn invalid_cases(valid: &EnvelopeHeader) -> Vec<(&'static str, InvalidEnvelopeCase)> {
    let case = |name: &'static str, desc: &str, f: &dyn Fn(&mut EnvelopeHeader)| {
        let mut h = valid.clone();
        f(&mut h);
        let e = h
            .validate(&Limits::DEFAULT)
            .expect_err("invalid fixture must fail validation");
        (
            name,
            InvalidEnvelopeCase {
                description: desc.into(),
                header: h,
                expected_error: e.kind().into(),
                expected_error_code: e.error_code(),
            },
        )
    };
    vec![
        case(
            "unsupported-envelope-version",
            "envelope_version from the future",
            &|h| h.envelope_version = 2,
        ),
        case(
            "zero-op-schema",
            "op_schema_version 0 is never valid",
            &|h| h.op_schema_version = 0,
        ),
        case(
            "self-dependency-cycle",
            "depends on its own device at the same seq",
            &|h| {
                h.depends_on.push(OpRef {
                    device_id: h.device_id.clone(),
                    device_seq: h.device_seq,
                })
            },
        ),
        case(
            "duplicate-dependency",
            "same causal reference twice",
            &|h| {
                let d = h.depends_on[0].clone();
                h.depends_on.push(d)
            },
        ),
        case(
            "too-many-dependencies",
            "65 references with default max 64",
            &|h| {
                h.depends_on = (1..=65)
                    .map(|s| OpRef {
                        device_id: id(DEV_B),
                        device_seq: s,
                    })
                    .collect()
            },
        ),
        case(
            "uppercase-hash",
            "ciphertext_sha256 must be lowercase hex",
            &|h| h.ciphertext_sha256 = h.ciphertext_sha256.to_uppercase(),
        ),
        case("empty-ciphertext", "ciphertext_length 0", &|h| {
            h.ciphertext_length = 0
        }),
        case(
            "ciphertext-too-large",
            "ciphertext_length above default 1 MiB",
            &|h| h.ciphertext_length = Limits::DEFAULT.max_ciphertext_bytes_per_op + 1,
        ),
        case("padded-signature", "signature with base64 padding", &|h| {
            h.signature.push_str("==")
        }),
    ]
}

/// Generate every fixture file as `(file name, bytes)`, sorted by name.
pub fn generate() -> BTreeMap<String, Vec<u8>> {
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut put = |name: &str, bytes: Vec<u8>| {
        assert!(
            files.insert(name.to_owned(), bytes).is_none(),
            "duplicate fixture {name}"
        );
    };
    let mut json = |name: &str, s: String| put(name, s.into_bytes());
    let limits = Limits::DEFAULT;
    let server = InMemoryServer::with_secret(FIXTURE_SECRET, FIXTURE_INSTANCE_ID, limits);
    let account: AccountId = id(ACCOUNT);
    let dev_a: DeviceId = id(DEV_A);
    server.register_device(&account, &dev_a);
    server.register_device(&account, &id(DEV_B));

    json("meta.json", to_fixture_json(&server.meta()));
    json(
        "contract-manifest.json",
        to_fixture_json(&ContractManifest::current()),
    );
    json("error-codes.json", to_fixture_json(&error_code_table()));

    // Valid envelope with an unsorted depends_on (canonical bytes sort it).
    let ct_a = fake_ciphertext("op-a", 96);
    let ct_b = fake_ciphertext("op-b", 1962);
    let ct_c = fake_ciphertext("op-c", 40);
    let a = header_for(OP_A, DEV_A, 1, &[], &ct_a);
    let b = header_for(OP_B, DEV_A, 2, &[(DEV_B, 9), (DEV_A, 1)], &ct_b);
    json("envelope-header.valid.json", to_fixture_json(&b));
    json(
        "envelope-header.valid.signing-bytes.hex",
        hex::encode(&canonical_signing_bytes(&b)) + "\n",
    );
    json(
        "envelope-header.valid.aad-bytes.hex",
        hex::encode(&canonical_aad_bytes(&b)) + "\n",
    );
    for (name, case) in invalid_cases(&b) {
        json(
            &format!("envelope-header.invalid.{name}.json"),
            to_fixture_json(&case),
        );
    }

    // Push: first batch accepted.
    let batch1 = encode_json_batch(
        &[(a.clone(), ct_a.clone()), (b.clone(), ct_b.clone())],
        &limits,
    )
    .expect("fixture batch fits limits");
    let resp1 = server
        .push(&account, &dev_a, &batch1)
        .expect("fixture push");
    let mut raw = vec![("push-batch.bin", batch1)];
    json("push-response.accepted.json", to_fixture_json(&resp1));

    // Retry batch: duplicate of A, op_id reuse of B with other content,
    // integrity mismatch, and a new op C.
    let b_conflict_ct = fake_ciphertext("op-b-other", 64);
    let b_conflict = header_for(OP_B, DEV_A, 2, &[(DEV_A, 1)], &b_conflict_ct);
    let ct_bad = fake_ciphertext("op-d", 32);
    let mut bad = header_for(
        "0190c6f2-7b1a-7cc0-9d1e-000000000004",
        DEV_A,
        4,
        &[],
        &ct_bad,
    );
    bad.ciphertext_sha256 = sha256_hex(b"something else");
    let c = header_for(OP_C, DEV_A, 3, &[(DEV_A, 2)], &ct_c);
    let batch2 = encode_json_batch(
        &[
            (a.clone(), ct_a.clone()),
            (b_conflict, b_conflict_ct),
            (bad, ct_bad),
            (c, ct_c),
        ],
        &limits,
    )
    .expect("fixture batch fits limits");
    let resp2 = server
        .push(&account, &dev_a, &batch2)
        .expect("fixture push");
    raw.push(("push-batch.retry.bin", batch2));
    json("push-response.retry.json", to_fixture_json(&resp2));

    // Pull in pages of 2.
    let req1 = PullOpsRequest {
        cursor: None,
        limit: 2,
    };
    let page1 = server.pull(&account, &dev_a, &req1).expect("fixture pull");
    let req2 = PullOpsRequest {
        cursor: Some(page1.response.next_cursor.clone()),
        limit: 2,
    };
    let page2 = server.pull(&account, &dev_a, &req2).expect("fixture pull");
    json("pull-request.page1.json", to_fixture_json(&req1));
    json("pull-response.page1.json", to_fixture_json(&page1.response));
    json("pull-request.page2.json", to_fixture_json(&req2));
    json("pull-response.page2.json", to_fixture_json(&page2.response));
    json(
        "ack-request.json",
        to_fixture_json(&AckRequest {
            applied_cursor: page2.response.next_cursor.clone(),
        }),
    );
    server
        .ack(
            &account,
            &dev_a,
            &AckRequest {
                applied_cursor: page2.response.next_cursor.clone(),
            },
        )
        .expect("fixture ack");
    json(
        "usage-response.json",
        to_fixture_json(&server.usage(&account).expect("usage")),
    );
    json(
        "device-registration-request.json",
        to_fixture_json(&DeviceRegistrationRequest {
            device_id: None,
            signing_public_key: base64url::encode(&[0x11; 32]),
            encryption_public_key: base64url::encode(&[0x22; 32]),
            label: Some("Linux desktop".into()),
        }),
    );
    json(
        "device-registration-response.json",
        to_fixture_json(&DeviceRegistrationResponse {
            device_id: id(DEV_A),
        }),
    );

    // Error bodies observed from the fake server.
    server.expire_cursors_before(&account, 3);
    let expired = server.pull(&account, &dev_a, &req2).expect_err("expired");
    server.fail_next_requests(1, ErrorCode::RateLimited);
    let limited = server.usage(&account).expect_err("rate limited");
    for (name, e) in [("cursor-expired", expired), ("rate-limited", limited)] {
        let RequestError::Api(body) = e else {
            unreachable!("fixture errors are API errors")
        };
        json(&format!("error.{name}.json"), to_fixture_json(&body));
    }
    json(
        "error.upgrade-required.json",
        to_fixture_json(&ErrorBody::new(
            ErrorCode::UpgradeRequired,
            "client protocol 1.0 is older than the server minimum 2.0; upgrade the app",
            "req-upgrade",
        )),
    );

    // Sign-in and sessions (auth.v1). Tokens are fixed placeholders.
    json(
        "auth-config.json",
        to_fixture_json(&AuthConfig {
            issuer: "https://login.example.invalid/dex".into(),
            client_id: "penroam-app".into(),
            scopes: vec!["openid".into(), "email".into(), "offline_access".into()],
            redirect: REDIRECT_LOOPBACK.into(),
            loopback_ports: vec![],
            mobile_redirect_uri: None,
        }),
    );
    // A provider that needs every redirect URI registered exactly (protocol 1.2).
    json(
        "auth-config.loopback-ports.json",
        to_fixture_json(&AuthConfig {
            issuer: "https://login.example.invalid/".into(),
            client_id: "penroam-app".into(),
            scopes: vec!["openid".into(), "email".into()],
            redirect: REDIRECT_LOOPBACK_PORTS.into(),
            loopback_ports: vec![47011, 47012, 47013],
            mobile_redirect_uri: Some("ink.penroam.app://callback".into()),
        }),
    );
    json(
        "auth-session-request.json",
        to_fixture_json(&SessionRequest {
            id_token: "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJmaXh0dXJlIn0.c2lnbmF0dXJl".into(),
        }),
    );
    json(
        "auth-refresh-request.json",
        to_fixture_json(&RefreshRequest {
            refresh_token: "rt_fixture-refresh-token".into(),
        }),
    );
    for (name, device_id) in [
        ("auth-session-tokens.account.json", None),
        ("auth-session-tokens.device.json", Some(dev_a.clone())),
    ] {
        json(
            name,
            to_fixture_json(&SessionTokens {
                account_id: account.clone(),
                device_id,
                access_token: "at_fixture-access-token".into(),
                expires_in_s: 900,
                refresh_token: "rt_fixture-refresh-token".into(),
            }),
        );
    }
    json(
        "auth-device-challenge-request.json",
        to_fixture_json(&DeviceChallengeRequest {
            device_id: dev_a.clone(),
        }),
    );
    let challenge = base64url::encode(&[0x3c; 32]);
    json(
        "auth-device-challenge.json",
        to_fixture_json(&DeviceChallenge {
            challenge: challenge.clone(),
            expires_in_s: 120,
        }),
    );
    json(
        "auth-device-session-request.json",
        to_fixture_json(&DeviceSessionRequest {
            device_id: dev_a.clone(),
            challenge: challenge.clone(),
            signature: placeholder_signature(),
        }),
    );
    json(
        "auth-device-session.signing-bytes.hex",
        hex::encode(&canonical_device_auth_bytes(
            &account,
            &dev_a,
            &challenge,
            FIXTURE_INSTANCE_ID,
        )) + "\n",
    );

    raw.push(("pull-batch.page1.bin", page1.frame));
    raw.push(("pull-batch.page2.bin", page2.frame));
    for (name, bytes) in raw {
        files.insert(name.to_owned(), bytes);
    }
    files
}

/// Write all fixtures into `dir` (creating it). Stale `*.json`/`*.bin`/`*.hex`
/// files not produced by the generator are removed.
pub fn write(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let files = generate();
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let generated_kind = [".json", ".bin", ".hex"].iter().any(|e| name.ends_with(e));
        if p.is_file() && generated_kind && !files.contains_key(&name) {
            std::fs::remove_file(&p)?;
        }
    }
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes)?;
    }
    Ok(())
}

/// Compare `dir` against freshly generated fixtures. Returns a list of
/// human-readable differences (empty = identical).
pub fn check(dir: &Path) -> std::io::Result<Vec<String>> {
    let files = generate();
    let mut diffs = Vec::new();
    for (name, bytes) in &files {
        match std::fs::read(dir.join(name)) {
            Ok(existing) if &existing == bytes => {}
            Ok(existing) => {
                let first = existing
                    .iter()
                    .zip(bytes)
                    .position(|(a, b)| a != b)
                    .unwrap_or(existing.len().min(bytes.len()));
                diffs.push(format!(
                    "changed: {name} (committed {} bytes, generated {} bytes, first difference at byte {first})",
                    existing.len(),
                    bytes.len()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                diffs.push(format!("missing: {name}"))
            }
            Err(e) => return Err(e),
        }
    }
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let p = entry?.path();
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            if p.is_file() && !files.contains_key(&name) {
                diffs.push(format!("unexpected: {name}"));
            }
        }
    }
    diffs.sort();
    Ok(diffs)
}
