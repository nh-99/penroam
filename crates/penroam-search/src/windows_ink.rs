//! Windows on-device handwriting recognition (`Windows.UI.Input.Inking`).
//!
//! Windows ships stroke-based handwriting recognizers with its language features (for
//! example "Microsoft English (US) Handwriting Recognizer"). They run locally, need no
//! download from Penroam and see vector strokes, not pixels, so a region's strokes are passed
//! as they are: points in page points are scaled to device-independent pixels (96 per inch;
//! the recognizers are tuned for writing at its real size), with pressure, tilt and sample
//! timestamps. Each recognition result is one word with its text candidates and the strokes
//! it used, which map back to Penroam stroke ids for highlighting.
//!
//! The "language" of this provider is the recognizer's display name, as Windows reports it;
//! the platform API exposes no language tag. The engine reports no confidence.
//!
//! WinRT objects used here are not `Send`, so they are created per call on the calling
//! (indexing) thread. That costs well under a millisecond against tens of milliseconds of
//! recognition per line.

use crate::recognize::{RecognitionCandidate, RecognizeError, Recognizer, RecognizerInfo};
use crate::segment::{Rect, Region};
use std::collections::HashMap;
use uuid::Uuid;
use windows::Foundation::Point;
use windows::UI::Input::Inking::{
    InkPoint, InkRecognitionTarget, InkRecognizerContainer, InkStrokeBuilder, InkStrokeContainer,
};
use windows_collections::IIterable;

/// Points (1/72 in) to device-independent pixels (1/96 in).
const DIP_PER_PT: f64 = 96.0 / 72.0;

/// The Windows handwriting recognizers installed on this device.
pub struct WindowsInkRecognizer {
    info: RecognizerInfo,
}

fn engine(e: windows::core::Error) -> RecognizeError {
    RecognizeError::Engine(e.message().to_string())
}

/// Names of the installed handwriting recognizers (empty when none is installed or the API
/// is unavailable).
pub fn installed_recognizers() -> Vec<String> {
    let list = || -> windows::core::Result<Vec<String>> {
        let c = InkRecognizerContainer::new()?;
        let rs = c.GetRecognizers()?;
        let mut out = Vec::new();
        for r in rs {
            out.push(r.Name()?.to_string());
        }
        Ok(out)
    };
    list().unwrap_or_default()
}

impl WindowsInkRecognizer {
    /// The provider, or `None` when Windows has no handwriting recognizer installed.
    pub fn new() -> Option<Self> {
        let languages = installed_recognizers();
        if languages.is_empty() {
            return None;
        }
        Some(WindowsInkRecognizer {
            info: RecognizerInfo {
                id: "windows-ink".into(),
                model_version: "1".into(),
                name: "Windows handwriting recognition".into(),
                languages,
                license: "Part of Windows (installed with Windows language features); runs on \
                          this device"
                    .into(),
            },
        })
    }
}

impl Recognizer for WindowsInkRecognizer {
    fn info(&self) -> &RecognizerInfo {
        &self.info
    }

    fn recognize(
        &mut self,
        region: &Region<'_>,
        language: &str,
    ) -> Result<Vec<RecognitionCandidate>, RecognizeError> {
        let rc = InkRecognizerContainer::new().map_err(engine)?;
        let mut chosen = None;
        for r in rc.GetRecognizers().map_err(engine)? {
            if r.Name().map_err(engine)? == language {
                chosen = Some(r);
                break;
            }
        }
        let chosen = chosen.ok_or_else(|| RecognizeError::LanguageUnavailable(language.into()))?;
        rc.SetDefaultRecognizer(&chosen).map_err(engine)?;

        let builder = InkStrokeBuilder::new().map_err(engine)?;
        let container = InkStrokeContainer::new().map_err(engine)?;
        let mut by_id: HashMap<u32, Uuid> = HashMap::new();
        let t0 = region
            .strokes
            .iter()
            .filter_map(|s| s.points.first())
            .map(|p| p.time_us)
            .min()
            .unwrap_or(0);
        for s in &region.strokes {
            let mut pts = Vec::with_capacity(s.points.len());
            for p in &s.points {
                let ip = InkPoint::CreateInkPointWithTiltAndTimestamp(
                    Point {
                        X: (p.x * DIP_PER_PT) as f32,
                        Y: (p.y * DIP_PER_PT) as f32,
                    },
                    p.pressure.clamp(0.0, 1.0) as f32,
                    p.tilt_x.clamp(-90.0, 90.0) as f32,
                    p.tilt_y.clamp(-90.0, 90.0) as f32,
                    (p.time_us - t0).max(0) as u64,
                )
                .map_err(engine)?;
                pts.push(Some(ip));
            }
            let it: IIterable<InkPoint> = pts.into();
            let stroke = builder
                .CreateStrokeFromInkPoints(&it, windows_numerics::Matrix3x2::identity())
                .map_err(engine)?;
            container.AddStroke(&stroke).map_err(engine)?;
            by_id.insert(stroke.Id().map_err(engine)?, s.id);
        }
        let bounds: HashMap<Uuid, Rect> = region
            .strokes
            .iter()
            .map(|s| {
                let b = penroam_core::geometry::stroke_bounds(&s.points, s.width, s.pressure);
                (s.id, Rect::from_corners(b[0], b[1], b[2], b[3]))
            })
            .collect();
        let results = rc
            .RecognizeAsync(&container, InkRecognitionTarget::All)
            .map_err(engine)?
            .join()
            .map_err(engine)?;
        let mut out = Vec::new();
        for r in results {
            let mut texts = Vec::new();
            for t in r.GetTextCandidates().map_err(engine)? {
                let t = t.to_string();
                if !t.trim().is_empty() && !texts.contains(&t) {
                    texts.push(t);
                }
            }
            if texts.is_empty() {
                continue;
            }
            let mut ids = Vec::new();
            for s in r.GetStrokes().map_err(engine)? {
                if let Some(id) = by_id.get(&s.Id().map_err(engine)?) {
                    ids.push(*id);
                }
            }
            let b = ids
                .iter()
                .filter_map(|id| bounds.get(id))
                .copied()
                .reduce(|a, b| a.union(&b));
            let b = match b {
                Some(b) => b,
                None => {
                    let r = r.BoundingRect().map_err(engine)?;
                    Rect {
                        x: r.X as f64 / DIP_PER_PT,
                        y: r.Y as f64 / DIP_PER_PT,
                        w: r.Width as f64 / DIP_PER_PT,
                        h: r.Height as f64 / DIP_PER_PT,
                    }
                }
            };
            let text = texts.remove(0);
            out.push(RecognitionCandidate {
                text,
                alternates: texts,
                confidence: None,
                stroke_ids: ids,
                bounds: b,
            });
        }
        Ok(out)
    }
}
