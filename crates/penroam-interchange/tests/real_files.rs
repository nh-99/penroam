//! Runs a folder of real files through the importers and prints what happened to each. Third-party
//! samples have their own licences, so they aren't kept in this repository:
//!
//! ```text
//! PENROAM_IMPORT_CORPUS=/path/to/files cargo test -p penroam-interchange --test real_files -- --nocapture
//! ```
//!
//! With `PENROAM_IMPORT_PDF_DIR` set, each imported document is also written there as an annotated
//! PDF, to compare with what the original application draws.
//!
//! Without the variable this passes without doing anything. A file may be refused, but must never
//! panic, and whatever an importer accepts must pass the model's validation.

use penroam_core::validate::validate_page;
use penroam_core::{Page, Paper};
use penroam_interchange::{enex, jex, xournal, ImportedDocument};
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn import(ext: &str, bytes: &[u8]) -> Option<Result<ImportedDocument, String>> {
    let r = match ext {
        "xopp" | "xoj" => xournal::import_xournal(bytes, "real"),
        "enex" => enex::import_enex(bytes, "real"),
        "jex" => jex::import_jex(bytes, "real"),
        _ => return None,
    };
    Some(r.map_err(|e| e.to_string()))
}

/// Draws an imported document the way the app exports it.
fn write_pdf(d: &ImportedDocument, path: &Path) {
    let pages: Vec<Page> = d
        .pages
        .iter()
        .map(|ip| {
            let mut p = Page::new(
                Uuid::new_v4(),
                Paper::Custom,
                ip.size,
                ip.template.clone(),
                0,
            );
            p.elements = ip.elements.clone();
            p
        })
        .collect();
    let asset = |h: &str| {
        d.assets
            .iter()
            .find(|a| a.sha256 == h)
            .map(|a| a.bytes.clone())
    };
    if let Ok((bytes, _)) = penroam_pdf::export_annotated(&pages, &[], asset) {
        let _ = std::fs::write(path.with_extension("pdf"), bytes);
    }
}

#[test]
fn real_files() {
    let Some(root) = std::env::var_os("PENROAM_IMPORT_CORPUS") else {
        return;
    };
    let mut files = Vec::new();
    walk(Path::new(&root), &mut files);
    files.sort();
    let (mut ok, mut refused, mut bad) = (0, 0, Vec::new());
    for f in files {
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        let t = Instant::now();
        let res = std::panic::catch_unwind(|| import(&ext, &bytes));
        let name = f.strip_prefix(&root).unwrap_or(&f).display().to_string();
        match res {
            Err(_) => {
                println!("PANIC {name}");
                bad.push(name);
            }
            Ok(None) => {}
            Ok(Some(Err(e))) => {
                refused += 1;
                println!("REFUSED {name}: {e}");
            }
            Ok(Some(Ok(d))) => {
                ok += 1;
                for ip in &d.pages {
                    let mut p = Page::new(
                        Uuid::new_v4(),
                        Paper::Custom,
                        ip.size,
                        ip.template.clone(),
                        0,
                    );
                    p.elements = ip.elements.clone();
                    if let Err(e) = validate_page(&p) {
                        println!("INVALID {name}: {e}");
                        bad.push(name.clone());
                    }
                }
                if let Some(dir) = std::env::var_os("PENROAM_IMPORT_PDF_DIR") {
                    write_pdf(&d, &Path::new(&dir).join(name.replace(['/', ' '], "_")));
                }
                let k = d.report.kept;
                println!(
                    "OK {name}: {} pages, {} strokes, {} text, {} images, {:?}; lost: {:?}",
                    k.pages,
                    k.strokes,
                    k.text_blocks,
                    k.images,
                    t.elapsed(),
                    d.report.lost
                );
            }
        }
    }
    println!("{ok} imported, {refused} refused, {} bad", bad.len());
    assert!(bad.is_empty(), "{bad:?}");
}
