//! Pinned cryptographic vectors (test keys only). Any change to a
//! derivation, the nonce rule, the payload sealing or the signature input
//! changes these bytes and fails this test: that is a wire-visible change.
//!
//! Regenerate deliberately with `UPDATE_VECTORS=1 cargo test -p penroam-sync --test vectors`.

use penroam_sync::crypto::{self, DeviceKeys, EpochKey, RootSecret};
use penroam_sync_protocol::{
    base64url, canonical_aad_bytes, canonical_signing_bytes, hex, EnvelopeHeader,
};

const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/vectors/crypto-vectors.json"
);

fn compute() -> serde_json::Value {
    let root = RootSecret::from_bytes([0x11; 32]);
    let epoch = EpochKey([0x22; 32]);
    let dev = DeviceKeys::from_secret_string(&format!(
        "00000000-0000-4000-8000-000000000001\n{}\n{}",
        base64url::encode(&[0x33; 32]),
        base64url::encode(&[0x44; 32])
    ))
    .unwrap();
    let nb = uuid::Uuid::from_u128(0x0123_4567_89ab_4cde_8f01_2345_6789_abcd);
    let routing = epoch.routing_id(nb);
    let nb_key = epoch.notebook_key(&routing);
    let payload = b"penroam test payload".to_vec();
    let mut h = EnvelopeHeader {
        envelope_version: 1,
        op_id: penroam_sync_protocol::OpId::new("00000000-0000-4000-8000-0000000000aa").unwrap(),
        device_id: penroam_sync_protocol::DeviceId::new(dev.device_id.clone()).unwrap(),
        device_seq: 2,
        notebook_id: penroam_sync_protocol::NotebookRoutingId::new(routing.clone()).unwrap(),
        depends_on: vec![penroam_sync_protocol::OpRef {
            device_id: penroam_sync_protocol::DeviceId::new(dev.device_id.clone()).unwrap(),
            device_seq: 1,
        }],
        key_epoch: 1,
        op_schema_version: 1,
        ciphertext_sha256: "0".repeat(64),
        ciphertext_length: crypto::sealed_len(payload.len()) as u64,
        signature: String::new(),
    };
    let op_uuid = uuid::Uuid::parse_str(h.op_id.as_str()).unwrap();
    let ct = crypto::seal(
        &nb_key,
        op_uuid.as_bytes(),
        &canonical_aad_bytes(&h),
        &payload,
    );
    h.ciphertext_sha256 = crypto::sha256_hex(&ct);
    let sig = crypto::sign(&dev.signing, b"", &canonical_signing_bytes(&h));
    h.signature = base64url::encode(&sig);
    assert_eq!(
        crypto::open(&nb_key, &canonical_aad_bytes(&h), &ct).unwrap(),
        payload
    );
    serde_json::json!({
        "note": "test keys only; see docs/protocol/sync-protocol-v1.md §13-§14",
        "root_secret_hex": hex::encode(&[0x11; 32]),
        "recovery_code": root.recovery_code(),
        "account_public_key_b64": base64url::encode(&root.account_signing_key().verifying_key().to_bytes()),
        "recovery_public_key_b64": base64url::encode(&x25519_dalek::PublicKey::from(&root.recovery_dh()).to_bytes()),
        "epoch_key_hex": hex::encode(&epoch.0),
        "notebook_uuid": nb.to_string(),
        "routing_id": routing,
        "notebook_key_hex": hex::encode(&nb_key),
        "blob_key_hex": hex::encode(&epoch.blob_key(&routing)),
        "blob_id": epoch.blob_id(&routing, &"ab".repeat(32)),
        "device_signing_public_b64": base64url::encode(&dev.sign_pk()),
        "device_dh_public_b64": base64url::encode(&dev.dh_pk()),
        "device_fingerprint": dev.fingerprint(),
        "comparison_code": crypto::short_auth_string(&root.account_signing_key().verifying_key().to_bytes(), &dev.fingerprint()),
        "payload_hex": hex::encode(&payload),
        "header": h,
        "aad_hex": hex::encode(&canonical_aad_bytes(&h)),
        "ciphertext_hex": hex::encode(&ct),
        "signing_bytes_hex": hex::encode(&canonical_signing_bytes(&h)),
    })
}

#[test]
fn crypto_vectors_are_pinned() {
    let v = compute();
    let text = serde_json::to_string_pretty(&v).unwrap() + "\n";
    if std::env::var("UPDATE_VECTORS").is_ok() {
        std::fs::create_dir_all(std::path::Path::new(PATH).parent().unwrap()).unwrap();
        std::fs::write(PATH, &text).unwrap();
    }
    let pinned =
        std::fs::read_to_string(PATH).expect("vectors file (run with UPDATE_VECTORS=1 once)");
    assert_eq!(
        pinned, text,
        "crypto vectors changed: this is a wire-visible change"
    );
    // and the signature verifies with the published public key
    let h: EnvelopeHeader = serde_json::from_value(v["header"].clone()).unwrap();
    let pk = crypto::decode32(v["device_signing_public_b64"].as_str().unwrap()).unwrap();
    crypto::verify(
        &pk,
        b"",
        &canonical_signing_bytes(&h),
        &base64url::decode(&h.signature).unwrap(),
    )
    .unwrap();
}
