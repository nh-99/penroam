//! Fetching handwriting models from a server that offers `models.v1`.
//!
//! The server is only a mirror. What makes a download trustworthy is the Ed25519 signature on
//! the catalog, made offline and checked here against keys built into the app; the catalog in
//! turn pins each archive's SHA-256. So a compromised server, or anyone on the network, can
//! withhold a model but cannot get a different one run. That matters because an ONNX graph is
//! parsed and executed by a large native library, and the checksums inside a bundle only prove
//! the bundle is what the bundle says it is.
//!
//! Invariants:
//!
//! * [`verify_catalog`] refuses a catalog whose `sequence` is below the highest seen, so an old
//!   signed catalog cannot be replayed to push a device back to a model with a known problem.
//! * [`download`] writes to `<dest>.part` and moves it to `dest` only after the size and SHA-256
//!   match. `dest` therefore never holds a partial or unverified archive.
//! * No request here carries a token or device id; downloads are anonymous.
//!
//! Contract: §12c of `docs/protocol/sync-protocol-v1.md`, DTOs in
//! `penroam_sync_protocol::models`.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use penroam_sync_protocol::base64url;
use penroam_sync_protocol::models::{
    CatalogError, ModelCatalog, SignedCatalog, CATALOG_SIGNING_DOMAIN,
};

/// A public key allowed to sign catalogs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    /// Matched against [`SignedCatalog::key_id`]. Several keys can be trusted at once, so a key
    /// can be rotated across an app release.
    pub id: String,
    /// Ed25519 public key.
    pub public: [u8; 32],
}

impl TrustedKey {
    /// Reads a key from its base64url form.
    pub fn parse(id: &str, public_b64: &str) -> Option<TrustedKey> {
        let public = base64url::decode(public_b64).ok()?.try_into().ok()?;
        Some(TrustedKey {
            id: id.to_string(),
            public,
        })
    }
}

/// Why a catalog or an archive was refused, or could not be fetched.
#[derive(Debug, thiserror::Error)]
pub enum ModelFetchError {
    /// Signed by a key this app does not trust.
    #[error("the model catalog is signed by an unknown key")]
    UnknownKey,
    /// The signature does not match the catalog text.
    #[error("the model catalog's signature is not valid")]
    BadSignature,
    /// The catalog is older than one this device has already seen.
    #[error("the model catalog is older than the one this device has")]
    Rollback,
    /// Signature fine, content not.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The server could not be reached, or answered with this message.
    #[error("{0}")]
    Network(String),
    /// The server has no such archive.
    #[error("the server does not have this model")]
    NotFound,
    /// The download's size or SHA-256 is not what the catalog says.
    #[error("the downloaded model does not match the catalog")]
    Integrity,
    /// The download was cancelled.
    #[error("the download was cancelled")]
    Cancelled,
    /// A local file could not be written.
    #[error("could not save the model: {0}")]
    Io(#[from] std::io::Error),
}

fn signing_bytes(catalog: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(CATALOG_SIGNING_DOMAIN.len() + catalog.len());
    m.extend_from_slice(CATALOG_SIGNING_DOMAIN);
    m.extend_from_slice(catalog.as_bytes());
    m
}

/// Checks a catalog's signature, then its content and sequence. `min_sequence` is the highest
/// sequence this device has accepted (0 if none); the caller stores the new one.
///
/// The signature is checked first so that nothing unsigned reaches the JSON parser.
pub fn verify_catalog(
    signed: &SignedCatalog,
    keys: &[TrustedKey],
    min_sequence: u64,
) -> Result<ModelCatalog, ModelFetchError> {
    let key = keys
        .iter()
        .find(|k| k.id == signed.key_id)
        .ok_or(ModelFetchError::UnknownKey)?;
    let public = VerifyingKey::from_bytes(&key.public).map_err(|_| ModelFetchError::UnknownKey)?;
    let sig: [u8; 64] = base64url::decode(&signed.signature)
        .ok()
        .and_then(|s| s.try_into().ok())
        .ok_or(ModelFetchError::BadSignature)?;
    public
        .verify(
            &signing_bytes(&signed.catalog),
            &Signature::from_bytes(&sig),
        )
        .map_err(|_| ModelFetchError::BadSignature)?;
    let catalog = ModelCatalog::parse(&signed.catalog)?;
    if catalog.sequence < min_sequence {
        return Err(ModelFetchError::Rollback);
    }
    Ok(catalog)
}

/// A new signing key as `(secret seed, public key)`. Used by the release tooling; the seed must
/// stay offline.
pub fn generate_signing_key() -> Result<([u8; 32], [u8; 32]), getrandom::Error> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed)?;
    let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    Ok((seed, public))
}

/// Signs catalog text (the release tooling's half of [`verify_catalog`]).
pub fn sign_catalog(catalog: String, key_id: &str, seed: &[u8; 32]) -> SignedCatalog {
    let sig = SigningKey::from_bytes(seed).sign(&signing_bytes(&catalog));
    SignedCatalog {
        key_id: key_id.to_string(),
        signature: base64url::encode(&sig.to_bytes()),
        catalog,
    }
}

#[cfg(feature = "http")]
pub use http::{download, fetch_catalog};

#[cfg(feature = "http")]
mod http {
    use super::ModelFetchError;
    use penroam_sync_protocol::dto::capabilities;
    use penroam_sync_protocol::models::{CatalogModel, SignedCatalog, MAX_CATALOG_BYTES};
    use penroam_sync_protocol::{hex, routes, ServerMeta};
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// A slow link should finish a large model; this only bounds a transfer that never ends.
    const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

    fn agent(total: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(total))
            .timeout_connect(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn net(e: ureq::Error) -> ModelFetchError {
        ModelFetchError::Network(e.to_string())
    }

    fn join(base: &str, path: &str) -> String {
        format!("{}{path}", base.trim_end_matches('/'))
    }

    fn get_json(a: &ureq::Agent, url: &str) -> Result<Option<Vec<u8>>, ModelFetchError> {
        let r = a.get(url).call().map_err(net)?;
        match r.status().as_u16() {
            200 => r
                .into_body()
                .with_config()
                .limit(2 * MAX_CATALOG_BYTES as u64)
                .read_to_vec()
                .map(Some)
                .map_err(net),
            404 => Ok(None),
            s => Err(ModelFetchError::Network(format!("server answered {s}"))),
        }
    }

    /// The server's signed catalog, unverified. `None` when the server does not offer models or
    /// has not published any: that is not an error, the app simply has nothing to offer.
    pub fn fetch_catalog(base: &str) -> Result<Option<SignedCatalog>, ModelFetchError> {
        let a = agent(Duration::from_secs(60));
        let bad = |e: serde_json::Error| ModelFetchError::Network(e.to_string());
        let Some(meta) = get_json(&a, &join(base, routes::META))? else {
            return Ok(None);
        };
        let meta: ServerMeta = serde_json::from_slice(&meta).map_err(bad)?;
        if !meta.has_capability(capabilities::MODELS_V1) {
            return Ok(None);
        }
        match get_json(&a, &join(base, routes::MODELS_CATALOG))? {
            Some(b) => serde_json::from_slice(&b).map(Some).map_err(bad),
            None => Ok(None),
        }
    }

    /// Removes `path` unless told otherwise, so no exit leaves a `.part` file behind.
    struct PartFile {
        path: PathBuf,
        keep: bool,
    }

    impl Drop for PartFile {
        fn drop(&mut self) {
            if !self.keep {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    /// Downloads `model` to `dest`, verifying its size and SHA-256 against the catalog.
    /// `progress` gets the bytes received so far. Setting `cancel` stops within one read.
    pub fn download(
        base: &str,
        model: &CatalogModel,
        dest: &Path,
        cancel: &AtomicBool,
        mut progress: impl FnMut(u64),
    ) -> Result<(), ModelFetchError> {
        let url = join(
            base,
            &routes::MODELS_FILE.replace("{sha256}", &model.sha256),
        );
        let r = agent(DOWNLOAD_TIMEOUT).get(&url).call().map_err(net)?;
        match r.status().as_u16() {
            200 => {}
            404 => return Err(ModelFetchError::NotFound),
            s => return Err(ModelFetchError::Network(format!("server answered {s}"))),
        }
        // one byte over the expected size is enough to tell that it is wrong
        let mut body = r
            .into_body()
            .into_with_config()
            .limit(model.bytes + 1)
            .reader();
        let mut part = PartFile {
            path: dest.with_extension("part"),
            keep: false,
        };
        let mut file = std::fs::File::create(&part.path)?;
        let mut hash = Sha256::new();
        let mut received = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(ModelFetchError::Cancelled);
            }
            let n = match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return Err(ModelFetchError::Network(e.to_string())),
            };
            hash.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            received += n as u64;
            progress(received);
        }
        if received != model.bytes || hex::encode(&hash.finalize()) != model.sha256 {
            return Err(ModelFetchError::Integrity);
        }
        file.sync_all()?;
        drop(file);
        std::fs::rename(&part.path, dest)?;
        part.keep = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penroam_sync_protocol::models::{CATALOG_FORMAT, CATALOG_FORMAT_VERSION};

    fn text(sequence: u64) -> String {
        format!(
            r#"{{"format":"{CATALOG_FORMAT}","format_version":{CATALOG_FORMAT_VERSION},"sequence":{sequence},"models":[]}}"#
        )
    }

    fn keys() -> ([u8; 32], Vec<TrustedKey>) {
        let (seed, public) = generate_signing_key().unwrap();
        (
            seed,
            vec![TrustedKey {
                id: "k1".into(),
                public,
            }],
        )
    }

    #[test]
    fn accepts_a_signed_catalog_at_or_above_the_seen_sequence() {
        let (seed, keys) = keys();
        let s = sign_catalog(text(5), "k1", &seed);
        assert_eq!(verify_catalog(&s, &keys, 0).unwrap().sequence, 5);
        assert_eq!(verify_catalog(&s, &keys, 5).unwrap().sequence, 5);
    }

    #[test]
    fn refuses_a_replayed_older_catalog() {
        let (seed, keys) = keys();
        let s = sign_catalog(text(5), "k1", &seed);
        assert!(matches!(
            verify_catalog(&s, &keys, 6),
            Err(ModelFetchError::Rollback)
        ));
    }

    #[test]
    fn refuses_edited_text_foreign_keys_and_garbage_signatures() {
        let (seed, keys) = keys();
        let good = sign_catalog(text(5), "k1", &seed);

        let mut edited = good.clone();
        edited.catalog = text(6);
        assert!(matches!(
            verify_catalog(&edited, &keys, 0),
            Err(ModelFetchError::BadSignature)
        ));

        let mut garbage = good.clone();
        garbage.signature = "AAAA".into();
        assert!(matches!(
            verify_catalog(&garbage, &keys, 0),
            Err(ModelFetchError::BadSignature)
        ));

        // the right key id, signed by someone else
        let (other, _) = generate_signing_key().unwrap();
        assert!(matches!(
            verify_catalog(&sign_catalog(text(5), "k1", &other), &keys, 0),
            Err(ModelFetchError::BadSignature)
        ));

        assert!(matches!(
            verify_catalog(&sign_catalog(text(5), "k2", &seed), &keys, 0),
            Err(ModelFetchError::UnknownKey)
        ));
        assert!(matches!(
            verify_catalog(&good, &[], 0),
            Err(ModelFetchError::UnknownKey)
        ));
    }

    #[test]
    fn the_signature_does_not_cover_a_different_domain() {
        // a device signature over the same text must not pass as a catalog signature
        let (seed, keys) = keys();
        let raw = SigningKey::from_bytes(&seed).sign(text(5).as_bytes());
        let forged = SignedCatalog {
            key_id: "k1".into(),
            catalog: text(5),
            signature: base64url::encode(&raw.to_bytes()),
        };
        assert!(matches!(
            verify_catalog(&forged, &keys, 0),
            Err(ModelFetchError::BadSignature)
        ));
    }

    #[test]
    fn trusted_keys_parse_from_base64url() {
        let (_, public) = generate_signing_key().unwrap();
        let k = TrustedKey::parse("k", &base64url::encode(&public)).unwrap();
        assert_eq!(k.public, public);
        assert!(TrustedKey::parse("k", "short").is_none());
    }
}

#[cfg(all(test, feature = "http"))]
mod http_tests {
    use super::*;
    use penroam_sync_protocol::models::{CatalogModel, ACCESS_FREE};
    use penroam_sync_protocol::{hex, ProtocolVersion, ServerMeta};
    use sha2::{Digest, Sha256};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;

    /// Answers each request from `route(path)` as `(status, body)`, until the test ends.
    fn serve(route: impl Fn(&str) -> (u16, Vec<u8>) + Send + 'static) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut r = BufReader::new(s);
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                }
                let (status, body) = route(&path);
                let mut s = r.into_inner();
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(&body);
            }
        });
        url
    }

    fn meta(caps: &[&str]) -> Vec<u8> {
        serde_json::to_vec(&ServerMeta {
            min_client_protocol: ProtocolVersion::new(1, 0),
            max_client_protocol: ProtocolVersion::new(1, 4),
            capabilities: caps.iter().map(|c| c.to_string()).collect(),
            limits: Default::default(),
            server_instance_id: "test".into(),
        })
        .unwrap()
    }

    fn model_for(bytes: &[u8]) -> CatalogModel {
        CatalogModel {
            id: "m".into(),
            version: "1".into(),
            name: "M".into(),
            license: "MIT".into(),
            languages: vec![],
            sha256: hex::encode(&Sha256::digest(bytes)),
            bytes: bytes.len() as u64,
            model_format_version: 1,
            min_app_version: "0.0.0".into(),
            access: ACCESS_FREE.into(),
            recommended: true,
        }
    }

    #[test]
    fn a_server_without_the_capability_has_no_catalog() {
        let url = serve(|p| match p {
            "/v1/meta" => (200, meta(&["acks.v1"])),
            _ => panic!("must not ask for {p}"),
        });
        assert!(fetch_catalog(&url).unwrap().is_none());
    }

    #[test]
    fn a_server_with_nothing_published_has_no_catalog() {
        let url = serve(|p| match p {
            "/v1/meta" => (200, meta(&["models.v1"])),
            _ => (404, vec![]),
        });
        assert!(fetch_catalog(&url).unwrap().is_none());
    }

    #[test]
    fn fetches_a_published_catalog() {
        let (seed, _) = generate_signing_key().unwrap();
        let signed = sign_catalog("{}".into(), "k1", &seed);
        let body = serde_json::to_vec(&signed).unwrap();
        let url = serve(move |p| match p {
            "/v1/meta" => (200, meta(&["models.v1"])),
            "/v1/models/catalog" => (200, body.clone()),
            _ => (404, vec![]),
        });
        assert_eq!(fetch_catalog(&url).unwrap(), Some(signed));
    }

    #[test]
    fn an_outage_is_an_error_not_an_empty_catalog() {
        let url = serve(|_| (503, vec![]));
        assert!(matches!(
            fetch_catalog(&url),
            Err(ModelFetchError::Network(_))
        ));
    }

    #[test]
    fn downloads_and_reports_progress() {
        let bytes = vec![7u8; 300_000];
        let m = model_for(&bytes);
        let path = format!("/v1/models/files/{}", m.sha256);
        let url = serve(move |p| {
            assert_eq!(p, path);
            (200, bytes.clone())
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.penroam-model");
        let mut last = 0;
        download(&url, &m, &dest, &AtomicBool::new(false), |n| last = n).unwrap();
        assert_eq!(last, 300_000);
        assert_eq!(std::fs::read(&dest).unwrap().len(), 300_000);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no .part"
        );
    }

    fn assert_refused(served: Vec<u8>, m: &CatalogModel, want: fn(&ModelFetchError) -> bool) {
        let url = serve(move |_| (200, served.clone()));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.penroam-model");
        let e = download(&url, m, &dest, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(want(&e), "{e}");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "left a file"
        );
    }

    #[test]
    fn a_swapped_archive_leaves_nothing_behind() {
        let m = model_for(&[1u8; 1000]);
        assert_refused(vec![2u8; 1000], &m, |e| {
            matches!(e, ModelFetchError::Integrity)
        });
    }

    #[test]
    fn a_short_archive_is_refused() {
        let m = model_for(&[1u8; 1000]);
        assert_refused(vec![1u8; 999], &m, |e| {
            matches!(e, ModelFetchError::Integrity)
        });
    }

    #[test]
    fn an_oversized_archive_is_refused() {
        let m = model_for(&[1u8; 1000]);
        assert_refused(vec![1u8; 5000], &m, |e| {
            matches!(e, ModelFetchError::Network(_) | ModelFetchError::Integrity)
        });
    }

    #[test]
    fn cancelling_leaves_nothing_behind() {
        let bytes = vec![1u8; 1000];
        let m = model_for(&bytes);
        let url = serve(move |_| (200, bytes.clone()));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.penroam-model");
        let e = download(&url, &m, &dest, &AtomicBool::new(true), |_| {}).unwrap_err();
        assert!(matches!(e, ModelFetchError::Cancelled));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_missing_archive_is_reported_as_such() {
        let m = model_for(&[1u8; 10]);
        let url = serve(|_| (404, vec![]));
        let dir = tempfile::tempdir().unwrap();
        let e = download(
            &url,
            &m,
            &dir.path().join("m"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(e, ModelFetchError::NotFound));
    }
}
