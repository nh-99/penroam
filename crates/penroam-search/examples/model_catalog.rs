//! Release tooling for the handwriting model catalog: makes a signing key, and builds and signs
//! a catalog from `.penroam-model` archives.
//!
//! ```text
//! cargo run -p penroam-search --example model_catalog -- keygen <secret-key-file>
//! cargo run -p penroam-search --example model_catalog -- sign \
//!     --key <secret-key-file> --key-id <id> --sequence <n> --min-app <x.y.z> \
//!     --recommend <file.penroam-model> [--out catalog.signed.json] [other.penroam-model ...]
//! ```
//!
//! Each entry's id, version, name, licence and languages are read from the archive itself, so a
//! catalog cannot describe a model as something it is not. Whoever holds the secret key can
//! choose what runs on every device, so it must never reach the server that answers clients.
//! `--sequence` must exceed the last published catalog's, because clients refuse a lower one.
//! Hosted releases do not need `sign`: the service signs the model it publishes. It is for
//! servers that publish by hand. The format is §12c of `docs/protocol/sync-protocol-v1.md`.

use penroam_search::model;
use penroam_sync::models::{generate_signing_key, sign_catalog};
use penroam_sync_protocol::models::{
    CatalogModel, ModelCatalog, ACCESS_FREE, CATALOG_FORMAT, CATALOG_FORMAT_VERSION,
};
use penroam_sync_protocol::{base64url, hex};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

type Res<T> = Result<T, String>;

fn entry(path: &Path, recommended: bool, min_app: &str) -> Res<CatalogModel> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m = model::inspect_archive(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(CatalogModel {
        id: m.id,
        version: m.version,
        name: m.name,
        license: m.license,
        languages: m.languages,
        sha256: hex::encode(&Sha256::digest(&bytes)),
        bytes: bytes.len() as u64,
        model_format_version: m.format_version,
        min_app_version: min_app.to_string(),
        access: ACCESS_FREE.to_string(),
        recommended,
    })
}

fn keygen(path: &Path) -> Res<()> {
    use std::io::Write;
    let (seed, public) = generate_signing_key().map_err(|e| e.to_string())?;
    // create_new: never overwrite a key that devices may already trust
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut f = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(base64url::encode(&seed).as_bytes())
        .map_err(|e| e.to_string())?;
    println!(
        "public key (for TRUSTED_KEYS): {}",
        base64url::encode(&public)
    );
    Ok(())
}

fn sign(args: &[String]) -> Res<()> {
    let (mut key, mut key_id, mut sequence, mut min_app, mut recommend, mut out) = (
        None,
        None,
        None,
        None,
        None,
        PathBuf::from("catalog.signed.json"),
    );
    let mut others = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--key" => key = Some(PathBuf::from(value()?)),
            "--key-id" => key_id = Some(value()?),
            "--sequence" => sequence = Some(value()?.parse::<u64>().map_err(|e| e.to_string())?),
            "--min-app" => min_app = Some(value()?),
            "--recommend" => recommend = Some(PathBuf::from(value()?)),
            "--out" => out = PathBuf::from(value()?),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => others.push(PathBuf::from(s)),
        }
    }
    let missing = |n: &str| format!("{n} is required");
    let key = key.ok_or(missing("--key"))?;
    let key_id = key_id.ok_or(missing("--key-id"))?;
    let sequence = sequence.ok_or(missing("--sequence"))?;
    let min_app = min_app.ok_or(missing("--min-app"))?;
    let recommend = recommend.ok_or(missing("--recommend"))?;

    let mut models = vec![entry(&recommend, true, &min_app)?];
    for p in &others {
        models.push(entry(p, false, &min_app)?);
    }
    let catalog = ModelCatalog {
        format: CATALOG_FORMAT.to_string(),
        format_version: CATALOG_FORMAT_VERSION,
        sequence,
        models,
    };
    let text = serde_json::to_string(&catalog).map_err(|e| e.to_string())?;
    ModelCatalog::parse(&text).map_err(|e| e.to_string())?;
    let encoded = std::fs::read_to_string(&key).map_err(|e| e.to_string())?;
    let seed: [u8; 32] = base64url::decode(encoded.trim())
        .ok()
        .and_then(|s| s.try_into().ok())
        .ok_or("the key file does not hold a key")?;
    let signed = sign_catalog(text, &key_id, &seed);
    let json = serde_json::to_vec_pretty(&signed).map_err(|e| e.to_string())?;
    std::fs::write(&out, json).map_err(|e| e.to_string())?;
    println!("wrote {} ({} models)", out.display(), catalog.models.len());
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.split_first() {
        Some((cmd, [path])) if cmd == "keygen" => keygen(Path::new(path)),
        Some((cmd, rest)) if cmd == "sign" => sign(rest),
        _ => Err(
            "usage: model_catalog keygen <file> | model_catalog sign --key … (see the source)"
                .into(),
        ),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
