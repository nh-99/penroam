//! Joplin `.jex` exports: an uncompressed tar of Joplin's own item files.
//!
//! ```text
//! <id>.md                one item: title, blank line, body, blank line, `key: value` lines
//! resources/<id>.<ext>   the bytes of an attachment
//! ```
//!
//! The metadata key `type_` says what an item is: 1 note, 2 folder, 4 attachment, 5 tag.
//! Note bodies are Markdown, and refer to attachments as `![alt](:/<id>)`.
//!
//! This reader was written from the file layout as Joplin documents and writes it, and tested
//! with hand-written files.
//!
//! **Security.** Entry names come from the file: they only ever select an id out of an
//! in-memory table and are never used as paths. The tar is walked in memory with every length
//! checked against what is there, and images pass the same checks as other importers.

use crate::document::{
    pdf_attachment, ImportError, ImportLoss, ImportedAsset, ImportedDocument, Kept, LossLog,
};
use crate::enex::{place_image, Image};
use crate::images::{check_image, pixel_size};
use crate::limits;
use crate::text_import::{blocks, escape_markdown, paginate};
use penroam_core::validate::limits as model;
use penroam_core::Element;
use std::collections::{BTreeMap, HashMap, HashSet};

const BLOCK: usize = 512;
const NOTE: &str = "1";
const FOLDER: &str = "2";
const RESOURCE: &str = "4";
const TAG: &str = "5";
/// Marks a body reference to an attachment or another note.
const REF: &str = "(:/";

fn malformed(e: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("this Joplin export can't be read: {e}"))
}

/// A tar size or checksum field: octal, space or NUL padded.
fn octal(field: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(field).ok()?;
    let t = s.trim_matches(|c: char| c == '\0' || c == ' ');
    if t.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(t, 8).ok()
}

/// Walks the regular files of a tar held in memory.
fn entries(tar: &[u8]) -> Result<Vec<(String, &[u8])>, ImportError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + BLOCK <= tar.len() {
        let head = &tar[i..i + BLOCK];
        if head.iter().all(|b| *b == 0) {
            break;
        }
        let stored = octal(&head[148..156]).ok_or_else(|| malformed("damaged header"))?;
        let sum: usize = head
            .iter()
            .enumerate()
            .map(|(n, b)| {
                if (148..156).contains(&n) {
                    32
                } else {
                    *b as usize
                }
            })
            .sum();
        if sum != stored {
            return Err(malformed("damaged header"));
        }
        let size = octal(&head[124..136]).ok_or_else(|| malformed("damaged header"))?;
        let start = i + BLOCK;
        let end = start
            .checked_add(size)
            .filter(|e| *e <= tar.len())
            .ok_or_else(|| malformed("the file ends early"))?;
        if matches!(head[156], 0 | b'0') {
            let name_end = head[..100].iter().position(|b| *b == 0).unwrap_or(100);
            out.push((
                String::from_utf8_lossy(&head[..name_end]).into_owned(),
                &tar[start..end],
            ));
        }
        i = start + size.div_ceil(BLOCK) * BLOCK;
    }
    Ok(out)
}

/// One Joplin item, split into title, body and metadata.
struct Item {
    title: String,
    body: String,
    meta: HashMap<String, String>,
}

/// Splits an item file. The metadata is the run of `key: value` lines at the end, so a body
/// that happens to end in such a line stays intact only up to the last blank line.
fn parse_item(text: &str) -> Item {
    let text = text.replace("\r\n", "\n");
    let trimmed = text.trim_end();
    let (head, tail) = trimmed.rsplit_once("\n\n").unwrap_or(("", trimmed));
    let mut meta = HashMap::new();
    for line in tail.lines() {
        if let Some((k, v)) = line.split_once(": ").or_else(|| line.split_once(':')) {
            meta.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let (title, body) = head.split_once("\n\n").unwrap_or((head, ""));
    Item {
        title: title.lines().next().unwrap_or("").to_string(),
        body: body.to_string(),
        meta,
    }
}

/// Removes `![alt](:/id)` image references from a body, returning the ids in order. Links
/// to other notes and to attachments that aren't images stay as text.
fn take_image_refs(body: &str, is_image: &dyn Fn(&str) -> bool) -> (String, Vec<String>) {
    let mut out = String::with_capacity(body.len());
    let mut ids = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("![") {
        let after = &rest[open + 2..];
        let found = after.find(']').and_then(|close| {
            let tail = after[close + 1..].strip_prefix(REF)?;
            let end = tail.find(')')?;
            let id = &tail[..end];
            is_image(id).then(|| (id.to_string(), close + 1 + REF.len() + end + 1))
        });
        match found {
            Some((id, len)) => {
                out.push_str(&rest[..open]);
                ids.push(id);
                rest = &after[len..];
            }
            None => {
                out.push_str(&rest[..open + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, ids)
}

/// Imports a Joplin `.jex` export as one notebook, with `title` as its title. Notes are
/// ordered by folder and then title, and each starts a new page. Errors when the file is too
/// large or isn't a readable export; everything lost is listed in the report.
pub fn import_jex(bytes: &[u8], title: &str) -> Result<ImportedDocument, ImportError> {
    if bytes.len() as u64 > limits::MAX_JEX_BYTES {
        return Err(ImportError::TooLarge("over 1 GiB".into()));
    }
    let mut items = Vec::new();
    let mut files: HashMap<String, &[u8]> = HashMap::new();
    for (name, data) in entries(bytes)? {
        if let Some(rest) = name.strip_prefix("resources/") {
            let id = rest.split('.').next().unwrap_or("").to_string();
            files.insert(id, data);
        } else if name.ends_with(".md") {
            items.push(parse_item(&String::from_utf8_lossy(data)));
        }
    }
    let kind = |i: &Item| i.meta.get("type_").cloned().unwrap_or_default();
    let folders: BTreeMap<String, String> = items
        .iter()
        .filter(|i| kind(i) == FOLDER)
        .filter_map(|i| Some((i.meta.get("id")?.clone(), i.title.clone())))
        .collect();
    let images: HashMap<&str, &str> = items
        .iter()
        .filter(|i| kind(i) == RESOURCE)
        .filter_map(|i| Some((i.meta.get("id")?.as_str(), i.meta.get("mime")?.as_str())))
        .filter(|(_, mime)| mime.starts_with("image/"))
        .collect();
    let pdfs: Vec<&str> = items
        .iter()
        .filter(|i| kind(i) == RESOURCE)
        .filter(|i| i.meta.get("mime").is_some_and(|m| m == "application/pdf"))
        .filter_map(|i| i.meta.get("id").map(String::as_str))
        .collect();
    let mut notes: Vec<&Item> = items.iter().filter(|i| kind(i) == NOTE).collect();
    if notes.is_empty() {
        return Err(malformed("it has no notes"));
    }
    let folder_of = |i: &Item| {
        i.meta
            .get("parent_id")
            .and_then(|p| folders.get(p))
            .cloned()
            .unwrap_or_default()
    };
    notes.sort_by_key(|n| (folder_of(n), n.title.clone()));

    let mut log = LossLog::default();
    if folders.len() > 1 {
        log.note_once("All folders came in as one notebook; notes are in folder order.");
    }
    log.note_once("Note dates weren't imported.");
    let tags = items.iter().filter(|i| kind(i) == TAG).count();
    for _ in 0..tags {
        log.note("{n} tag(s) were not imported.");
    }
    let mut used = HashSet::new();
    let mut pages = Vec::new();
    let mut assets: BTreeMap<String, ImportedAsset> = BTreeMap::new();
    let mut total = 0usize;
    for note in notes {
        if note
            .meta
            .get("encryption_applied")
            .is_some_and(|v| v == "1")
        {
            log.note("{n} note(s) were encrypted and were left out.");
            continue;
        }
        let (body, ids) = take_image_refs(&note.body, &|id| images.contains_key(id));
        let heading = escape_markdown(&note.title);
        // a Joplin to-do is a note with a tick box
        let text = match (
            note.meta.get("is_todo").map(String::as_str),
            note.meta.get("todo_completed"),
        ) {
            (Some("1"), Some(done)) if done != "0" && !done.is_empty() => {
                format!("- [x] {heading}\n\n{body}")
            }
            (Some("1"), _) => format!("- [ ] {heading}\n\n{body}"),
            _ => format!("# {heading}\n\n{body}"),
        };
        for _ in 0..text.matches(REF).count() {
            log.note("{n} link(s) to other notes or attachments no longer lead anywhere.");
        }
        total += text.len();
        if total > limits::MAX_TEXT_FILE {
            return Err(ImportError::TooLarge("over 64 MiB of text".into()));
        }
        pages.extend(paginate(blocks(&text))?);
        for id in ids {
            used.insert(id.clone());
            let Some(bytes) = files.get(id.as_str()) else {
                log.note("{n} image(s) are missing from the export.");
                continue;
            };
            let (Ok(media), Some((width, height))) = (check_image(bytes), pixel_size(bytes)) else {
                log.note("{n} image(s) were damaged, of an unsupported type or too large, and were left out.");
                continue;
            };
            let asset = ImportedAsset::new(bytes.to_vec(), media);
            let image = Image {
                asset: asset.sha256.clone(),
                width,
                height,
            };
            assets.entry(asset.sha256.clone()).or_insert(asset);
            place_image(&mut pages, &image)?;
            log.note("{n} image(s) moved to the end of their note.");
        }
        for id in &pdfs {
            if !note.body.contains(&format!("{REF}{id})")) {
                continue;
            }
            let Some(attached) = files.get(*id).and_then(|b| pdf_attachment(b.to_vec())) else {
                continue;
            };
            let (asset, pdf_pages) = attached;
            assets.entry(asset.sha256.clone()).or_insert(asset);
            pages.extend(pdf_pages);
            used.insert(id.to_string());
            log.note_once("PDF attachments became pages after their note.");
        }
        if pages.len() > model::MAX_PAGES {
            return Err(ImportError::TooLarge("more than 10 000 pages".into()));
        }
    }
    if pages.is_empty() {
        return Err(malformed("it has no readable notes"));
    }
    for _ in files.keys().filter(|k| !used.contains(*k)) {
        log.note("{n} attachment(s) were not imported, because they aren't images or no note shows them.");
    }
    let mut kept = Kept {
        pages: pages.len() as u32,
        ..Kept::default()
    };
    for e in pages.iter().flat_map(|p| &p.elements) {
        match e {
            Element::Text(_) => kept.text_blocks += 1,
            Element::Image(_) => kept.images += 1,
            Element::Ink(_) => kept.strokes += 1,
        }
    }
    Ok(ImportedDocument {
        title: title.to_string(),
        pages,
        assets: assets.into_values().collect(),
        report: ImportLoss {
            format: "Joplin".into(),
            kept,
            lost: log.sentences(),
        },
    })
}
