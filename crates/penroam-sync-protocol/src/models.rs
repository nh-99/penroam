//! Handwriting model catalog DTOs (capability `models.v1`, protocol 1.4).
//!
//! A server that distributes recognition models publishes one **signed catalog**. It lists the
//! `.penroam-model` archives on offer; each archive is then fetched by its SHA-256. Both routes
//! are unauthenticated, so a client can download a model without an account and without telling
//! the server who it is.
//!
//! The server is not trusted with the models' contents: the catalog is signed offline, and
//! clients accept it only under a public key built into the app. The signature covers the
//! catalog's exact bytes, which is why [`SignedCatalog::catalog`] carries JSON text and not a
//! nested object: nothing has to be re-serialized to check it. The archive's SHA-256 is inside
//! that signed text. This crate does no cryptography; `penroam_sync::models` verifies.
//!
//! Rules that keep older clients correct as the catalog grows:
//!
//! * A client offers only entries it can use: see [`CatalogModel::usable`]. An unknown
//!   [`CatalogModel::access`] value means "not for this client", so models that need a paid plan
//!   can be listed later without older apps trying, and failing, to download them.
//! * Unknown JSON fields are ignored.
//! * [`ModelCatalog::sequence`] only grows, so a replayed older catalog can be refused.
//!
//! The normative description is §12c of `docs/protocol/sync-protocol-v1.md`.

use crate::hex;
use serde::{Deserialize, Serialize};

/// The only [`ModelCatalog::format`] value.
pub const CATALOG_FORMAT: &str = "penroam-model-catalog";
/// Highest [`ModelCatalog::format_version`] this crate reads.
pub const CATALOG_FORMAT_VERSION: u32 = 1;
/// Domain separator: what an Ed25519 signature covers is this, then the catalog text.
pub const CATALOG_SIGNING_DOMAIN: &[u8] = b"penroam/model-catalog/v1\0";
/// [`CatalogModel::access`] value of a model anyone may download.
pub const ACCESS_FREE: &str = "free";
/// Largest catalog text accepted, in bytes.
pub const MAX_CATALOG_BYTES: usize = 1 << 19;
/// Largest model archive a catalog may list, in bytes.
pub const MAX_MODEL_BYTES: u64 = 512 << 20;

/// Why a catalog is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// Not JSON, or a field has the wrong type.
    #[error("invalid model catalog: {0}")]
    Malformed(String),
    /// [`ModelCatalog::format`] is not [`CATALOG_FORMAT`].
    #[error("not a model catalog")]
    WrongFormat,
    /// [`ModelCatalog::format_version`] is newer than this client reads.
    #[error("model catalog format {0} is newer than this version of Penroam supports")]
    TooNew(u32),
    /// An entry breaks a rule (the message names it).
    #[error("invalid model catalog entry: {0}")]
    BadEntry(String),
}

/// `GET /v1/models/catalog` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCatalog {
    /// Which public key signed this.
    pub key_id: String,
    /// The [`ModelCatalog`] as JSON text. Signed byte for byte.
    pub catalog: String,
    /// Base64url Ed25519 signature over [`CATALOG_SIGNING_DOMAIN`] then `catalog`.
    pub signature: String,
}

/// The models on offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalog {
    /// Always [`CATALOG_FORMAT`].
    pub format: String,
    /// Format of this document.
    pub format_version: u32,
    /// Grows with every published catalog; clients refuse anything lower than they have seen.
    pub sequence: u64,
    /// Every model on offer, in order of preference.
    pub models: Vec<CatalogModel>,
}

/// One downloadable model archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    /// Model id, as in the bundle's `model.json`.
    pub id: String,
    /// Model version, as in the bundle's `model.json`. Compared for equality only, so a catalog
    /// can move clients back to an earlier model.
    pub version: String,
    /// Name shown to the user.
    pub name: String,
    /// Licence text shown to the user.
    pub license: String,
    /// Languages the model reads.
    pub languages: Vec<String>,
    /// Lowercase hex SHA-256 of the archive; also its address on the server.
    pub sha256: String,
    /// Archive size in bytes.
    pub bytes: u64,
    /// The bundle's `formatVersion`; clients skip models newer than they can read.
    pub model_format_version: u32,
    /// Oldest app version that can run this model (`major.minor.patch`).
    pub min_app_version: String,
    /// Who may download this: [`ACCESS_FREE`], or a value a later protocol defines.
    pub access: String,
    /// Offered to devices that have no model, and as the update for those that do.
    pub recommended: bool,
}

impl ModelCatalog {
    /// Parses and validates catalog text.
    pub fn parse(text: &str) -> Result<ModelCatalog, CatalogError> {
        if text.len() > MAX_CATALOG_BYTES {
            return Err(CatalogError::Malformed("catalog is too large".into()));
        }
        let c: ModelCatalog =
            serde_json::from_str(text).map_err(|e| CatalogError::Malformed(e.to_string()))?;
        if c.format != CATALOG_FORMAT {
            return Err(CatalogError::WrongFormat);
        }
        if c.format_version > CATALOG_FORMAT_VERSION {
            return Err(CatalogError::TooNew(c.format_version));
        }
        c.models.iter().try_for_each(CatalogModel::validate)?;
        Ok(c)
    }

    /// The model to offer: the first recommended entry this client can use.
    pub fn recommended(&self, app_version: &str, model_format: u32) -> Option<&CatalogModel> {
        self.models
            .iter()
            .find(|m| m.recommended && m.usable(app_version, model_format))
    }
}

impl CatalogModel {
    /// Checks the fields clients rely on. The id ends up in a file path once the archive is
    /// installed, and the hash in a URL.
    pub fn validate(&self) -> Result<(), CatalogError> {
        let bad = |m: &str| CatalogError::BadEntry(format!("{}: {m}", self.id));
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 128
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !id_ok {
            return Err(bad("bad id"));
        }
        if !hex::is_sha256_hex(&self.sha256) {
            return Err(bad("bad sha256"));
        }
        if self.bytes == 0 || self.bytes > MAX_MODEL_BYTES {
            return Err(bad("bad size"));
        }
        if parse_version(&self.min_app_version).is_none() {
            return Err(bad("bad min_app_version"));
        }
        Ok(())
    }

    /// Whether this client can use the model: anyone may download it, the app is new enough and
    /// the app reads its bundle format.
    pub fn usable(&self, app_version: &str, model_format: u32) -> bool {
        let new_enough = match (
            parse_version(app_version),
            parse_version(&self.min_app_version),
        ) {
            (Some(app), Some(min)) => app >= min,
            _ => false,
        };
        self.access == ACCESS_FREE && new_enough && self.model_format_version <= model_format
    }
}

/// `major.minor.patch`, ignoring any `-pre` or `+build` suffix.
fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u32>().ok());
    let r = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> CatalogModel {
        CatalogModel {
            id: "penroam-handwriting-en".into(),
            version: "2026.11.0".into(),
            name: "Handwriting (English)".into(),
            license: "MIT".into(),
            languages: vec!["en".into()],
            sha256: "ab".repeat(32),
            bytes: 1000,
            model_format_version: 1,
            min_app_version: "0.2.0".into(),
            access: ACCESS_FREE.into(),
            recommended: true,
        }
    }

    fn catalog(models: Vec<CatalogModel>) -> ModelCatalog {
        ModelCatalog {
            format: CATALOG_FORMAT.into(),
            format_version: 1,
            sequence: 1,
            models,
        }
    }

    #[test]
    fn round_trips_and_ignores_unknown_fields() {
        let c = catalog(vec![model()]);
        let mut v = serde_json::to_value(&c).unwrap();
        v["from_the_future"] = serde_json::json!(true);
        v["models"][0]["also_new"] = serde_json::json!(1);
        assert_eq!(ModelCatalog::parse(&v.to_string()).unwrap(), c);
    }

    #[test]
    fn refuses_other_formats_and_bad_entries() {
        let text = |c: &ModelCatalog| serde_json::to_string(c).unwrap();
        let mut c = catalog(vec![]);
        c.format = "x".into();
        assert_eq!(
            ModelCatalog::parse(&text(&c)),
            Err(CatalogError::WrongFormat)
        );
        let mut c = catalog(vec![]);
        c.format_version = 2;
        assert_eq!(ModelCatalog::parse(&text(&c)), Err(CatalogError::TooNew(2)));
        assert!(ModelCatalog::parse("not json").is_err());

        let breakers: [fn(&mut CatalogModel); 6] = [
            |m| m.id = "../evil".into(),
            |m| m.id = String::new(),
            |m| m.sha256 = "ABC".into(),
            |m| m.bytes = 0,
            |m| m.bytes = MAX_MODEL_BYTES + 1,
            |m| m.min_app_version = "soon".into(),
        ];
        for b in breakers {
            let mut m = model();
            b(&mut m);
            assert!(matches!(
                ModelCatalog::parse(&text(&catalog(vec![m]))),
                Err(CatalogError::BadEntry(_))
            ));
        }
    }

    #[test]
    fn offers_only_what_this_client_can_use() {
        let mut paid = model();
        paid.access = "plan".into();
        paid.version = "paid".into();
        let mut too_new_app = model();
        too_new_app.min_app_version = "9.0.0".into();
        too_new_app.version = "new-app".into();
        let mut too_new_format = model();
        too_new_format.model_format_version = 2;
        too_new_format.version = "new-format".into();
        let mut not_recommended = model();
        not_recommended.recommended = false;
        not_recommended.version = "old".into();
        let c = catalog(vec![
            paid,
            too_new_app,
            too_new_format,
            not_recommended,
            model(),
        ]);
        assert_eq!(
            c.recommended("0.2.0", 1).map(|m| m.version.as_str()),
            Some("2026.11.0")
        );
        assert!(c.recommended("0.1.9", 1).is_none(), "app too old");
        assert!(c.recommended("garbage", 1).is_none());
    }

    #[test]
    fn versions_compare_numerically() {
        let mut m = model();
        m.min_app_version = "0.10.0".into();
        assert!(m.usable("0.10.0", 1));
        assert!(m.usable("0.10.1-beta.1", 1));
        assert!(!m.usable("0.9.9", 1));
    }
}
