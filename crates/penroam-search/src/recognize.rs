//! Recognition providers: the interface between handwriting regions and whatever turns ink
//! into text.
//!
//! A provider receives one [`Region`] (vector strokes, in page points, in writing order) and
//! returns [`RecognitionCandidate`]s: usually one per recognized word, each with the strokes it
//! came from and their bounds, so a search hit can highlight exactly that ink. Providers run on
//! the device; this crate has no network code and a provider must not add any.
//!
//! Every provider describes itself with a [`RecognizerInfo`]. Its `id` and `model_version` are
//! stored with each recognized entry, so changing either re-recognizes the library in the
//! background while the old entries stay searchable.
//!
//! Implementations:
//! * [`ScriptedRecognizer`]: deterministic, for tests and the evaluation tool's plumbing.
//! * `windows_ink::WindowsInkRecognizer` (feature `windows-ink`, Windows only): the Windows
//!   handwriting recognizers, stroke based, installed with the OS language packs.

use crate::segment::{Rect, Region};
use std::collections::HashMap;
use uuid::Uuid;

/// What a provider is, for the index, the settings screen and the model inventory.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecognizerInfo {
    /// Stable machine id, e.g. `windows-ink`.
    pub id: String,
    /// Version of the model or engine; a change triggers re-recognition.
    pub model_version: String,
    /// Human-readable name.
    pub name: String,
    /// BCP 47 language tags it can recognize.
    pub languages: Vec<String>,
    /// Where the model comes from and under which terms (shown to the user).
    pub license: String,
}

/// One recognized piece of a region, normally a word.
#[derive(Debug, Clone, PartialEq)]
pub struct RecognitionCandidate {
    /// Best text.
    pub text: String,
    /// Other readings, best first (kept searchable at a lower rank).
    pub alternates: Vec<String>,
    /// 0–1 when the provider reports one.
    pub confidence: Option<f32>,
    /// The strokes this text came from (a subset of the region's).
    pub stroke_ids: Vec<Uuid>,
    /// Bounds of those strokes in page points.
    pub bounds: Rect,
}

/// Why a region could not be recognized. The region is retried on the next index pass, so
/// none of these lose data.
#[derive(Debug, thiserror::Error)]
pub enum RecognizeError {
    /// No recognizer for the requested language is installed.
    #[error("no handwriting recognizer for {0} is installed")]
    LanguageUnavailable(String),
    /// The platform engine failed.
    #[error("handwriting recognition failed: {0}")]
    Engine(String),
}

/// A handwriting recognition provider. Implementations are called from one background thread
/// at a time and must never block on the network.
pub trait Recognizer: Send {
    /// Describes the provider.
    fn info(&self) -> &RecognizerInfo;

    /// Recognizes one region in `language` (a BCP 47 tag from [`RecognizerInfo::languages`]).
    /// An empty result means nothing legible was found, which is cached like any result.
    fn recognize(
        &mut self,
        region: &Region<'_>,
        language: &str,
    ) -> Result<Vec<RecognitionCandidate>, RecognizeError>;
}

/// Deterministic test provider: each stroke id maps to a word; the strokes of a region that
/// map to the same word (consecutively, in writing order) become one candidate.
#[derive(Debug, Default)]
pub struct ScriptedRecognizer {
    info: Option<RecognizerInfo>,
    words: HashMap<Uuid, String>,
    /// How many regions have been recognized (to assert that unchanged regions are reused).
    pub calls: usize,
}

impl ScriptedRecognizer {
    /// A provider with id `scripted` and the given model version.
    pub fn new(model_version: &str) -> Self {
        ScriptedRecognizer {
            info: Some(RecognizerInfo {
                id: "scripted".into(),
                model_version: model_version.into(),
                name: "Scripted test recognizer".into(),
                languages: vec!["en-US".into()],
                license: "test only".into(),
            }),
            words: HashMap::new(),
            calls: 0,
        }
    }

    /// Declares that `strokes` together read as `word`.
    pub fn word(&mut self, word: &str, strokes: &[Uuid]) {
        for s in strokes {
            self.words.insert(*s, word.to_string());
        }
    }
}

impl Recognizer for ScriptedRecognizer {
    fn info(&self) -> &RecognizerInfo {
        self.info.as_ref().expect("constructed with new()")
    }

    fn recognize(
        &mut self,
        region: &Region<'_>,
        language: &str,
    ) -> Result<Vec<RecognitionCandidate>, RecognizeError> {
        if !self.info().languages.iter().any(|l| l == language) {
            return Err(RecognizeError::LanguageUnavailable(language.into()));
        }
        self.calls += 1;
        let mut out: Vec<RecognitionCandidate> = Vec::new();
        for s in &region.strokes {
            let Some(w) = self.words.get(&s.id) else {
                continue;
            };
            let b = penroam_core::geometry::stroke_bounds(&s.points, s.width, s.pressure);
            let r = Rect::from_corners(b[0], b[1], b[2], b[3]);
            match out.last_mut() {
                Some(c) if &c.text == w => {
                    c.stroke_ids.push(s.id);
                    c.bounds = c.bounds.union(&r);
                }
                _ => out.push(RecognitionCandidate {
                    text: w.clone(),
                    alternates: vec![],
                    confidence: Some(1.0),
                    stroke_ids: vec![s.id],
                    bounds: r,
                }),
            }
        }
        Ok(out)
    }
}

/// A platform recognizer with the Penroam model as fallback. The platform recognizer is used
/// whenever it supports the requested language; a region it fails on (engine error) is
/// recognized by the fallback, as is every region when the platform has no recognizer or no
/// recognizer for that language. The fallback runs in its own first language.
///
/// The combined [`RecognizerInfo`] names both providers in `id` and `model_version`, so
/// installing, removing or upgrading either one re-recognizes the library.
pub struct ChainRecognizer {
    primary: Option<Box<dyn Recognizer>>,
    fallback: Option<Box<dyn Recognizer>>,
    info: RecognizerInfo,
}

impl ChainRecognizer {
    /// Combines the providers; `None` when there is neither.
    pub fn new(
        primary: Option<Box<dyn Recognizer>>,
        fallback: Option<Box<dyn Recognizer>>,
    ) -> Option<ChainRecognizer> {
        let info = match (&primary, &fallback) {
            (None, None) => return None,
            (Some(p), None) => p.info().clone(),
            (None, Some(f)) => f.info().clone(),
            (Some(p), Some(f)) => {
                let (p, f) = (p.info(), f.info());
                let mut languages = p.languages.clone();
                languages.extend(
                    f.languages
                        .iter()
                        .filter(|l| !p.languages.contains(l))
                        .cloned(),
                );
                RecognizerInfo {
                    id: format!("{}+{}", p.id, f.id),
                    model_version: format!("{}+{}", p.model_version, f.model_version),
                    name: format!("{} (with {} as fallback)", p.name, f.name),
                    languages,
                    license: format!("{} {}", p.license, f.license),
                }
            }
        };
        Some(ChainRecognizer {
            primary,
            fallback,
            info,
        })
    }

    /// The platform recognizer's description, if there is one.
    pub fn primary_info(&self) -> Option<&RecognizerInfo> {
        self.primary.as_ref().map(|p| p.info())
    }

    /// The fallback model's description, if one is installed.
    pub fn fallback_info(&self) -> Option<&RecognizerInfo> {
        self.fallback.as_ref().map(|f| f.info())
    }

    fn run_fallback(
        &mut self,
        region: &Region<'_>,
        language: &str,
        primary_error: Option<RecognizeError>,
    ) -> Result<Vec<RecognitionCandidate>, RecognizeError> {
        match self.fallback.as_mut() {
            Some(f) => {
                let lang = if f.info().languages.iter().any(|l| l == language) {
                    language.to_string()
                } else {
                    f.info().languages.first().cloned().unwrap_or_default()
                };
                f.recognize(region, &lang)
            }
            None => Err(primary_error
                .unwrap_or_else(|| RecognizeError::LanguageUnavailable(language.into()))),
        }
    }
}

impl Recognizer for ChainRecognizer {
    fn info(&self) -> &RecognizerInfo {
        &self.info
    }

    fn recognize(
        &mut self,
        region: &Region<'_>,
        language: &str,
    ) -> Result<Vec<RecognitionCandidate>, RecognizeError> {
        let primary_speaks = self
            .primary
            .as_ref()
            .is_some_and(|p| p.info().languages.iter().any(|l| l == language));
        if primary_speaks {
            let p = self.primary.as_mut().expect("checked");
            match p.recognize(region, language) {
                Ok(c) => return Ok(c),
                Err(e) => return self.run_fallback(region, language, Some(e)),
            }
        }
        self.run_fallback(region, language, None)
    }
}

/// A provider that always fails, for testing fallback paths.
#[doc(hidden)]
pub struct FailingRecognizer(pub RecognizerInfo);

impl Recognizer for FailingRecognizer {
    fn info(&self) -> &RecognizerInfo {
        &self.0
    }

    fn recognize(
        &mut self,
        _: &Region<'_>,
        _: &str,
    ) -> Result<Vec<RecognitionCandidate>, RecognizeError> {
        Err(RecognizeError::Engine("broken".into()))
    }
}
