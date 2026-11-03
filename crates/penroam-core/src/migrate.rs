//! Versioned schema migrations and the canonical stroke document import.
//!
//! Every canonical JSON document carries `schemaVersion`. Notebooks and pages
//! have **independent** schema versions ([`NOTEBOOK_SCHEMA_VERSION`],
//! [`PAGE_SCHEMA_VERSION`]); neither is derived from the binary file format
//! version or the operation schema version. Loading goes through
//! [`page_from_value`] / [`notebook_from_value`], which run the migration
//! chain `v(n) -> v(n+1)` on the raw JSON before deserialising (read old,
//! write current). Files newer than this build are refused, never truncated.
//! Storage backs up the original bytes before it writes a migrated file.

use crate::model::*;
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("{kind} schema version {found} is newer than this build supports ({supported}); please update Penroam")]
    TooNew {
        kind: &'static str,
        found: u64,
        supported: u32,
    },
    #[error("missing or invalid schemaVersion")]
    NoVersion,
    #[error("malformed document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a canonical stroke document")]
    NotStrokeDocument,
}

type Step = fn(Value) -> Result<Value, MigrateError>;

/// `MIGRATIONS[i]` upgrades schema version `i + 1` to `i + 2`. Append steps,
/// never edit old ones.
const PAGE_MIGRATIONS: &[Step] = &[];
const NOTEBOOK_MIGRATIONS: &[Step] = &[notebook_v1_to_v2];

/// Notebook 1 → 2: adds `orderKeys` reproducing `pageOrder` exactly (trashed
/// pages get keys at their former index). Purely additive and deterministic.
fn notebook_v1_to_v2(mut v: Value) -> Result<Value, MigrateError> {
    v["schemaVersion"] = Value::from(2);
    let mut nb: Notebook = serde_json::from_value(v)?;
    nb.rebuild_order_keys();
    Ok(serde_json::to_value(nb)?)
}

/// Schema version of a raw document (no migration).
pub fn schema_version_of(v: &Value) -> Option<u64> {
    v.get("schemaVersion").and_then(Value::as_u64)
}

fn run_chain(
    mut v: Value,
    chain: &[Step],
    current: u32,
    kind: &'static str,
) -> Result<Value, MigrateError> {
    debug_assert_eq!(chain.len() + 1, current as usize);
    let found = schema_version_of(&v).ok_or(MigrateError::NoVersion)?;
    if found == 0 {
        return Err(MigrateError::NoVersion);
    }
    if found > current as u64 {
        return Err(MigrateError::TooNew {
            kind,
            found,
            supported: current,
        });
    }
    for step in &chain[(found as usize - 1)..] {
        v = step(v)?;
    }
    Ok(v)
}

pub fn page_from_value(v: Value) -> Result<Page, MigrateError> {
    Ok(serde_json::from_value(run_chain(
        v,
        PAGE_MIGRATIONS,
        PAGE_SCHEMA_VERSION,
        "page",
    )?)?)
}

pub fn notebook_from_value(v: Value) -> Result<Notebook, MigrateError> {
    Ok(serde_json::from_value(run_chain(
        v,
        NOTEBOOK_MIGRATIONS,
        NOTEBOOK_SCHEMA_VERSION,
        "notebook",
    )?)?)
}

pub fn page_from_slice(b: &[u8]) -> Result<Page, MigrateError> {
    page_from_value(serde_json::from_slice(b)?)
}

pub fn notebook_from_slice(b: &[u8]) -> Result<Notebook, MigrateError> {
    notebook_from_value(serde_json::from_slice(b)?)
}

/// Converts a canonical stroke document (`{"format":"m00-strokes",
/// "version":1, strokes:[...]}`, or a bare stroke array; the format string
/// is a stable wire identifier) into ink elements. Coordinates are kept 1:1
/// (document units = points at zoom 1); `brush.width/color/pressure`,
/// `source`, the full `input` map and every point field are preserved.
/// String ids that are not UUIDs get a fresh UUID and the original id is
/// kept in `input.m00Id` (stable key).
pub fn stroke_doc_to_elements(doc: &Value, first_z: i64) -> Result<Vec<Element>, MigrateError> {
    let arr = match doc {
        Value::Array(a) => a,
        Value::Object(o) => {
            if o.get("format").and_then(Value::as_str) != Some("m00-strokes") {
                return Err(MigrateError::NotStrokeDocument);
            }
            o.get("strokes")
                .and_then(Value::as_array)
                .ok_or(MigrateError::NotStrokeDocument)?
        }
        _ => return Err(MigrateError::NotStrokeDocument),
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, s) in arr.iter().enumerate() {
        let empty = Map::new();
        let brush = s.get("brush").and_then(Value::as_object).unwrap_or(&empty);
        let mut input = s
            .get("input")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let raw_id = s.get("id").and_then(Value::as_str).unwrap_or("");
        let id = match Uuid::parse_str(raw_id) {
            Ok(u) => u,
            Err(_) => {
                if !raw_id.is_empty() {
                    input.insert("m00Id".into(), Value::String(raw_id.to_string()));
                }
                Uuid::new_v4()
            }
        };
        let points = s
            .get("points")
            .and_then(Value::as_array)
            .map(|pts| {
                pts.iter()
                    .map(|p| {
                        let f = |k: &str, d: f64| p.get(k).and_then(Value::as_f64).unwrap_or(d);
                        InkPoint {
                            x: f("x", 0.0),
                            y: f("y", 0.0),
                            pressure: f("pressure", 0.5).clamp(0.0, 1.0),
                            tilt_x: f("tiltX", 0.0),
                            tilt_y: f("tiltY", 0.0),
                            time_us: p.get("timeUs").and_then(Value::as_i64).unwrap_or(0),
                            twist: p.get("twist").and_then(Value::as_f64),
                            flags: None,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if points.is_empty() {
            continue;
        }
        let mut extra = Map::new();
        if let Some(t) = brush
            .get("type")
            .and_then(Value::as_str)
            .filter(|t| *t != "pen")
        {
            extra.insert("m00BrushType".into(), Value::String(t.to_string()));
        }
        out.push(Element::Ink(InkStroke {
            id,
            z: first_z + i as i64,
            tool: InkTool::Pen,
            color: brush
                .get("color")
                .and_then(Value::as_str)
                .filter(|c| crate::validate::is_color(c))
                .unwrap_or("#000000")
                .to_lowercase(),
            opacity: 1.0,
            width: brush.get("width").and_then(Value::as_f64).unwrap_or(3.0),
            pressure: brush
                .get("pressure")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            source: s
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            input,
            points,
            extra,
        }));
    }
    Ok(out)
}

/// Converts ink strokes back to the canonical stroke document (used by the
/// native ink adapters, which exchange strokes in that format).
pub fn elements_to_stroke_doc(strokes: &[&InkStroke]) -> Value {
    let arr: Vec<Value> = strokes
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id.to_string(),
                "source": s.source,
                "input": s.input,
                "brush": { "type": "pen", "width": s.width, "color": s.color, "pressure": s.pressure },
                "points": s.points.iter().map(|p| serde_json::json!({
                    "x": p.x, "y": p.y, "pressure": p.pressure, "tiltX": p.tilt_x, "tiltY": p.tilt_y, "timeUs": p.time_us
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "format": "m00-strokes", "version": 1, "meta": {}, "strokes": arr })
}
