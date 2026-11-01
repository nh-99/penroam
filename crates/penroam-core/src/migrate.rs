//! Versioned schema migrations and the M00 → M01 stroke import.
//!
//! Every canonical file carries `schemaVersion`. Loading goes through
//! [`page_from_value`] / [`notebook_from_value`], which run the migration
//! chain `v(n) -> v(n+1)` on the raw JSON before deserialising. Version 1 is
//! the first Penroam schema; files newer than this build are refused (never
//! silently truncated).

use crate::model::*;
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("unsupported schema version {found} (this build reads up to {SCHEMA_VERSION})")]
    TooNew { found: u64 },
    #[error("missing or invalid schemaVersion")]
    NoVersion,
    #[error("malformed document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not an M00 stroke document")]
    NotM00,
}

type Step = fn(Value) -> Result<Value, MigrateError>;

/// `MIGRATIONS[i]` upgrades schema version `i + 1` to `i + 2`. Empty while
/// only version 1 exists; add steps here, never edit old ones.
const PAGE_MIGRATIONS: &[Step] = &[];
const NOTEBOOK_MIGRATIONS: &[Step] = &[];

fn run_chain(mut v: Value, chain: &[Step]) -> Result<Value, MigrateError> {
    let found = v.get("schemaVersion").and_then(Value::as_u64).ok_or(MigrateError::NoVersion)?;
    if found == 0 {
        return Err(MigrateError::NoVersion);
    }
    if found > SCHEMA_VERSION as u64 {
        return Err(MigrateError::TooNew { found });
    }
    for step in &chain[(found as usize - 1)..] {
        v = step(v)?;
    }
    Ok(v)
}

pub fn page_from_value(v: Value) -> Result<Page, MigrateError> {
    Ok(serde_json::from_value(run_chain(v, PAGE_MIGRATIONS)?)?)
}

pub fn notebook_from_value(v: Value) -> Result<Notebook, MigrateError> {
    Ok(serde_json::from_value(run_chain(v, NOTEBOOK_MIGRATIONS)?)?)
}

pub fn page_from_slice(b: &[u8]) -> Result<Page, MigrateError> {
    page_from_value(serde_json::from_slice(b)?)
}

pub fn notebook_from_slice(b: &[u8]) -> Result<Notebook, MigrateError> {
    notebook_from_value(serde_json::from_slice(b)?)
}

/// Converts an M00 canonical stroke document (`{"format":"m00-strokes",
/// "version":1, strokes:[...]}`, or a bare stroke array) into M01 ink
/// elements. Coordinates are kept 1:1 (M00 document units = points at zoom
/// 1); `brush.width/color/pressure`, `source`, the full `input` map and every
/// point field are preserved. M00 string ids that are not UUIDs get a fresh
/// UUID and the original id is kept in `input.m00Id`.
pub fn m00_strokes_to_elements(doc: &Value, first_z: i64) -> Result<Vec<Element>, MigrateError> {
    let arr = match doc {
        Value::Array(a) => a,
        Value::Object(o) => {
            if o.get("format").and_then(Value::as_str) != Some("m00-strokes") {
                return Err(MigrateError::NotM00);
            }
            o.get("strokes").and_then(Value::as_array).ok_or(MigrateError::NotM00)?
        }
        _ => return Err(MigrateError::NotM00),
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, s) in arr.iter().enumerate() {
        let empty = Map::new();
        let brush = s.get("brush").and_then(Value::as_object).unwrap_or(&empty);
        let mut input = s.get("input").and_then(Value::as_object).cloned().unwrap_or_default();
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
        if let Some(t) = brush.get("type").and_then(Value::as_str).filter(|t| *t != "pen") {
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
            pressure: brush.get("pressure").and_then(Value::as_bool).unwrap_or(true),
            source: s.get("source").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            input,
            points,
            extra,
        }));
    }
    Ok(out)
}

/// Converts M01 strokes back to the M00 canonical document (used by the
/// native ink adapters, which still speak the M00 format).
pub fn elements_to_m00(strokes: &[&InkStroke]) -> Value {
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
