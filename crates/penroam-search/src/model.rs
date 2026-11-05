//! The Penroam handwriting model: an optional, image-based line recognizer that is used where
//! the platform has no handwriting recognizer, and as a fallback when the platform's fails.
//!
//! A model is a **bundle directory** described by `model.json` (format:
//! `docs/formats/handwriting-model.md`): one ONNX graph that maps a rendered line
//! ([`crate::raster`]) to per-frame character scores, decoded here with greedy CTC. The
//! bundle declares its licence, size and training provenance, and lists the SHA-256 of every
//! file; [`ModelBundle::open`] refuses a bundle whose files do not match, so a damaged or
//! swapped file is never run.
//!
//! Inference needs ONNX Runtime and is compiled only with the `onnx-model` feature
//! (`OnnxRecognizer`). The bundle format, verification and decoding are always available
//! (and tested without a runtime).
//!
//! Word bounds come from the decoder's frame positions: every frame covers a fixed slice of
//! the rendered line, so a word's characters map back to an x range on the page, and the
//! strokes whose horizontal centre lies in that range are the word's strokes.

use crate::raster::{GrayImage, RasterSpec};
use crate::recognize::RecognitionCandidate;
use crate::segment::{Rect, Region};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The only `format` value this build reads.
pub const FORMAT: &str = "penroam-handwriting-model";
/// Highest `formatVersion` this build reads.
pub const FORMAT_VERSION: u32 = 1;

/// Why a model bundle cannot be used. Search keeps working without it.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// `model.json` or a listed file is missing or unreadable.
    #[error("handwriting model: cannot read {0}")]
    Io(String),
    /// `model.json` is not a valid manifest.
    #[error("handwriting model: invalid model.json: {0}")]
    Manifest(String),
    /// A newer format than this build understands.
    #[error("handwriting model format {0} is newer than this version of Penroam supports")]
    TooNew(u32),
    /// A file's size or SHA-256 differs from the manifest.
    #[error("handwriting model file {0} does not match its checksum")]
    Checksum(String),
    /// ONNX Runtime failed to load or run the graph.
    #[error("handwriting model runtime error: {0}")]
    Runtime(String),
}

/// One file of the bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Lowercase hex SHA-256.
    pub sha256: String,
    /// Size in bytes (shown to the user before installing).
    pub bytes: u64,
}

/// The graph's tensors and how to read its output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSpec {
    /// Bundle file holding the ONNX graph.
    pub file: String,
    /// Input tensor name: `float32 [1, channels, height, width]`, `(value / 255 − mean) / std`,
    /// where `value` is the pixel's coverage, or `255 − coverage` with [`GraphSpec::dark_ink`].
    pub input: String,
    /// Output tensor name: `float32 [1, frames, classes]`.
    pub output: String,
    /// Whether the output is already log-probabilities (`true`) or raw logits.
    pub log_probs: bool,
    /// The output is probabilities, neither log-probabilities nor logits. Overrides
    /// [`GraphSpec::log_probs`].
    #[serde(default)]
    pub probabilities: bool,
    /// Input channels: 1, or 3 for a graph made for colour images, which gets the same
    /// grayscale in each.
    #[serde(default = "one_channel")]
    pub channels: u32,
    /// The graph reads dark ink on a light page, not white on black.
    #[serde(default)]
    pub dark_ink: bool,
    /// Input normalisation mean.
    pub mean: f32,
    /// Input normalisation standard deviation.
    pub std: f32,
}

fn one_channel() -> u32 {
    1
}

/// `model.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Always [`FORMAT`].
    pub format: String,
    /// Format version (≤ [`FORMAT_VERSION`]).
    pub format_version: u32,
    /// Stable model id, e.g. `penroam-handwriting-en`.
    pub id: String,
    /// Model version; a change re-recognizes the library.
    pub version: String,
    /// Human-readable name.
    pub name: String,
    /// Licence of the weights (shown to the user).
    pub license: String,
    /// BCP 47 language tags.
    pub languages: Vec<String>,
    /// Decoder; only `ctc-greedy` exists.
    pub decoder: String,
    /// How lines are rendered for this model.
    pub raster: RasterSpec,
    /// The graph.
    pub graph: GraphSpec,
    /// Class index of the CTC blank.
    pub blank: usize,
    /// Characters for class indices (index `i` → `charset[i]`; the blank's entry is ignored).
    pub charset: Vec<String>,
    /// Every file in the bundle except `model.json`.
    pub files: BTreeMap<String, FileEntry>,
    /// Free-form provenance (training run, data sources). Shown, never interpreted.
    #[serde(default)]
    pub provenance: serde_json::Value,
}

impl Manifest {
    /// Total size of the bundle's files in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.files.values().map(|f| f.bytes).sum()
    }
}

/// A verified bundle on disk.
#[derive(Debug, Clone)]
pub struct ModelBundle {
    /// Bundle directory.
    pub dir: PathBuf,
    /// Its manifest.
    pub manifest: Manifest,
}

fn sha256_file(path: &Path) -> Result<(String, u64), ModelError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)
        .map_err(|e| ModelError::Io(format!("{}: {e}", path.display())))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut n = 0u64;
    loop {
        let k = f
            .read(&mut buf)
            .map_err(|e| ModelError::Io(format!("{}: {e}", path.display())))?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((h.finalize().iter().map(|b| format!("{b:02x}")).collect(), n))
}

impl ModelBundle {
    /// Reads `model.json` without checking the files (for showing size and licence before
    /// installing).
    pub fn read_manifest(dir: &Path) -> Result<Manifest, ModelError> {
        let path = dir.join("model.json");
        let bytes =
            std::fs::read(&path).map_err(|e| ModelError::Io(format!("{}: {e}", path.display())))?;
        parse_manifest(&bytes)
    }

    /// Opens a bundle and verifies every listed file's size and SHA-256.
    pub fn open(dir: &Path) -> Result<ModelBundle, ModelError> {
        let manifest = Self::read_manifest(dir)?;
        for (name, entry) in &manifest.files {
            let (sha, n) = sha256_file(&dir.join(name))?;
            if n != entry.bytes || !sha.eq_ignore_ascii_case(&entry.sha256) {
                return Err(ModelError::Checksum(name.clone()));
            }
        }
        Ok(ModelBundle {
            dir: dir.to_path_buf(),
            manifest,
        })
    }

    /// Path of the ONNX graph.
    pub fn graph_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.graph.file)
    }
}

/// Parses and validates `model.json` (format, version, decoder, file names).
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, ModelError> {
    let m: Manifest =
        serde_json::from_slice(bytes).map_err(|e| ModelError::Manifest(e.to_string()))?;
    if m.format != FORMAT {
        return Err(ModelError::Manifest(format!(
            "unknown format {:?}",
            m.format
        )));
    }
    if m.format_version > FORMAT_VERSION {
        return Err(ModelError::TooNew(m.format_version));
    }
    if m.format_version == 0 || m.decoder != "ctc-greedy" {
        return Err(ModelError::Manifest(format!(
            "unsupported decoder {:?}",
            m.decoder
        )));
    }
    if m.blank >= m.charset.len() || !m.files.contains_key(&m.graph.file) {
        return Err(ModelError::Manifest(
            "blank or graph file not listed".into(),
        ));
    }
    if m.graph.channels != 1 && m.graph.channels != 3 {
        return Err(ModelError::Manifest(format!(
            "unsupported channel count {}",
            m.graph.channels
        )));
    }
    if m.raster.height < 8 || m.raster.max_width < m.raster.height || m.graph.std <= 0.0 {
        return Err(ModelError::Manifest(
            "unusable raster or normalisation".into(),
        ));
    }
    for name in m.files.keys() {
        // bundle-relative plain names only: no traversal out of the directory
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(ModelError::Manifest(format!("bad file name {name:?}")));
        }
    }
    Ok(m)
}

/// Largest model archive accepted by [`install_archive`] (uncompressed).
pub const MAX_ARCHIVE_BYTES: u64 = 512 << 20;

/// File extension of a packed model bundle (a zip of the bundle directory's files).
pub const ARCHIVE_EXTENSION: &str = "penroam-model";

/// Installs a packed bundle (`.penroam-model`: a zip holding `model.json` and the files it
/// lists, all at the top level) into `models_dir/<model id>/`, replacing an installed model
/// with the same id. The files are extracted to a staging directory and verified with
/// [`ModelBundle::open`] before anything installed is touched, so a bad archive leaves the
/// current model in place.
pub fn install_archive(bytes: &[u8], models_dir: &Path) -> Result<ModelBundle, ModelError> {
    use std::io::Read;
    let io = |e: std::io::Error| ModelError::Io(e.to_string());
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| ModelError::Manifest(format!("not a model archive: {e}")))?;
    std::fs::create_dir_all(models_dir).map_err(io)?;
    let staging = models_dir.join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(io)?;
    let result = (|| {
        let mut total = 0u64;
        for i in 0..zip.len() {
            let mut f = zip
                .by_index(i)
                .map_err(|e| ModelError::Manifest(e.to_string()))?;
            if f.is_dir() {
                continue;
            }
            let name = f.name().to_string();
            if name.is_empty() || name.contains(['/', '\\', ':']) || name.starts_with('.') {
                return Err(ModelError::Manifest(format!(
                    "bad file name {name:?} in archive"
                )));
            }
            total += f.size();
            if total > MAX_ARCHIVE_BYTES {
                return Err(ModelError::Manifest("archive is too large".into()));
            }
            let mut buf = Vec::with_capacity(f.size() as usize);
            (&mut f)
                .take(MAX_ARCHIVE_BYTES + 1)
                .read_to_end(&mut buf)
                .map_err(io)?;
            std::fs::write(staging.join(&name), &buf).map_err(io)?;
        }
        let bundle = ModelBundle::open(&staging)?;
        // the manifest lists every file; drop anything extra the archive carried
        for e in std::fs::read_dir(&staging).map_err(io)?.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n != "model.json" && !bundle.manifest.files.contains_key(&n) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let id = &bundle.manifest.id;
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ModelError::Manifest(format!("bad model id {id:?}")));
        }
        let target = models_dir.join(id);
        let old = models_dir.join(format!(".old-{id}"));
        let _ = std::fs::remove_dir_all(&old);
        if target.exists() {
            std::fs::rename(&target, &old).map_err(io)?;
        }
        std::fs::rename(&staging, &target).map_err(io)?;
        let _ = std::fs::remove_dir_all(&old);
        Ok(ModelBundle {
            dir: target,
            manifest: bundle.manifest,
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Reads `model.json` from a packed bundle without installing anything (to show the user the
/// model's name, size and licence first).
pub fn inspect_archive(bytes: &[u8]) -> Result<Manifest, ModelError> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| ModelError::Manifest(format!("not a model archive: {e}")))?;
    let mut f = zip
        .by_name("model.json")
        .map_err(|_| ModelError::Manifest("the archive has no model.json".into()))?;
    let mut buf = Vec::new();
    (&mut f)
        .take(1 << 20)
        .read_to_end(&mut buf)
        .map_err(|e| ModelError::Io(e.to_string()))?;
    parse_manifest(&buf)
}

/// The installed model bundles under `models_dir` whose manifest reads (files are verified
/// only when a bundle is opened for use), sorted by id.
pub fn installed(models_dir: &Path) -> Vec<(PathBuf, Manifest)> {
    let mut out: Vec<(PathBuf, Manifest)> = std::fs::read_dir(models_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            ModelBundle::read_manifest(&e.path())
                .ok()
                .map(|m| (e.path(), m))
        })
        .collect();
    out.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    out
}

/// The model input for an image: `(value / 255 − mean) / std`, row-major
/// `[1, channels, H, W]`, with every channel the same (see [`GraphSpec`]).
pub fn input_tensor(img: &GrayImage, g: &GraphSpec) -> Vec<f32> {
    let plane: Vec<f32> = img
        .pixels
        .iter()
        .map(|&p| {
            let value = if g.dark_ink { 255 - p } else { p };
            (value as f32 / 255.0 - g.mean) / g.std
        })
        .collect();
    plane.repeat(g.channels as usize)
}

/// Probabilities as the log-probabilities [`ctc_greedy`] takes. Zero stays finite.
pub fn log_of_probabilities(scores: &[f32]) -> Vec<f32> {
    scores.iter().map(|p| p.max(1e-9).ln()).collect()
}

/// One decoded character: its text and the frame it was emitted at.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedChar {
    /// The character (from the charset).
    pub text: String,
    /// Frame index.
    pub frame: usize,
    /// Its probability at that frame.
    pub prob: f32,
}

/// Greedy CTC decoding of `[frames × classes]` scores: best class per frame, repeats
/// collapsed, blanks dropped. `log_probs` tells whether the scores are already
/// log-probabilities; otherwise a softmax is applied per frame.
pub fn ctc_greedy(
    scores: &[f32],
    classes: usize,
    blank: usize,
    charset: &[String],
    log_probs: bool,
) -> Vec<DecodedChar> {
    let mut out = Vec::new();
    let mut prev = usize::MAX;
    for (t, frame) in scores.chunks_exact(classes).enumerate() {
        let (best, &v) = frame
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .expect("classes > 0");
        if best != blank && best != prev {
            let prob = if log_probs {
                v.exp()
            } else {
                let sum: f32 = frame.iter().map(|x| (x - v).exp()).sum();
                1.0 / sum
            };
            out.push(DecodedChar {
                text: charset.get(best).cloned().unwrap_or_default(),
                frame: t,
                prob,
            });
        }
        prev = best;
    }
    out
}

/// A recognized word located in a rendered line: `left`/`right` are image x coordinates (px).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageWord {
    /// Best reading.
    pub text: String,
    /// Other readings.
    pub alternates: Vec<String>,
    /// Confidence, when known.
    pub confidence: Option<f32>,
    /// Left edge in the rendered image.
    pub left: f64,
    /// Right edge in the rendered image.
    pub right: f64,
}

/// Splits greedy-CTC characters into words (on whitespace) located by their frames: frame
/// `t` covers image x `[t, t + 1) · image_width / frames`.
pub fn ctc_words(chars: &[DecodedChar], frames: usize, image_width: u32) -> Vec<ImageWord> {
    if frames == 0 {
        return vec![];
    }
    let px = image_width as f64 / frames as f64;
    let mut out: Vec<(Vec<&DecodedChar>,)> = vec![(vec![],)];
    for c in chars {
        if c.text.trim().is_empty() {
            if !out.last().expect("non-empty").0.is_empty() {
                out.push((vec![],));
            }
        } else {
            out.last_mut().expect("non-empty").0.push(c);
        }
    }
    out.into_iter()
        .filter(|(w,)| !w.is_empty())
        .map(|(w,)| ImageWord {
            text: w.iter().map(|c| c.text.as_str()).collect(),
            alternates: vec![],
            confidence: Some(w.iter().map(|c| c.prob).sum::<f32>() / w.len() as f32),
            left: w[0].frame as f64 * px,
            right: (w[w.len() - 1].frame as f64 + 1.0) * px,
        })
        .collect()
}

/// Maps words located in a line rendered with `spec` (see [`crate::raster`]) back to the
/// page. The boundary between two neighbouring words is half-way between them; each stroke
/// belongs to the word whose span holds its horizontal centre, and a word's bounds are its
/// strokes' bounds (or its span, when no stroke centre falls inside).
pub fn words_on_page(
    words: &[ImageWord],
    region: &Region<'_>,
    spec: &RasterSpec,
) -> Vec<RecognitionCandidate> {
    // the same scale as the rasterizer (raster.rs, steps 1–3)
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for s in &region.strokes {
        for p in &s.points {
            x0 = x0.min(p.x);
            x1 = x1.max(p.x);
            y0 = y0.min(p.y);
            y1 = y1.max(p.y);
        }
    }
    if x0 > x1 {
        return vec![];
    }
    let pad = spec.padding as f64;
    let (w, h) = (x1 - x0, y1 - y0);
    let mut s = (spec.height as f64 - 2.0 * pad) / h.max(8.0);
    if w * s + 2.0 * pad > spec.max_width as f64 {
        s = (spec.max_width as f64 - 2.0 * pad) / w;
    }
    let page_x = |img_x: f64| x0 + (img_x - pad) / s;
    let n = words.len();
    let mut out = Vec::with_capacity(n);
    for (i, word) in words.iter().enumerate() {
        let left = if i == 0 {
            f64::MIN
        } else {
            page_x((words[i - 1].right + word.left) / 2.0)
        };
        let right = if i + 1 == n {
            f64::MAX
        } else {
            page_x((word.right + words[i + 1].left) / 2.0)
        };
        let mut ids = Vec::new();
        let mut bounds: Option<Rect> = None;
        for st in &region.strokes {
            let (mut a, mut b) = (f64::MAX, f64::MIN);
            for p in &st.points {
                a = a.min(p.x);
                b = b.max(p.x);
            }
            let c = (a + b) / 2.0;
            if c >= left && c < right {
                ids.push(st.id);
                let sb = penroam_core::geometry::stroke_bounds(&st.points, st.width, st.pressure);
                let r = Rect::from_corners(sb[0], sb[1], sb[2], sb[3]);
                bounds = Some(bounds.map_or(r, |u| u.union(&r)));
            }
        }
        let bounds = bounds.unwrap_or_else(|| {
            let (l, r) = (page_x(word.left).max(x0), page_x(word.right).min(x1));
            Rect::from_corners(l, y0, r.max(l), y1)
        });
        out.push(RecognitionCandidate {
            text: word.text.clone(),
            alternates: word.alternates.clone(),
            confidence: word.confidence,
            stroke_ids: ids,
            bounds,
        });
    }
    out
}

#[cfg(feature = "onnx-model")]
pub use onnx::OnnxRecognizer;

#[cfg(feature = "onnx-model")]
mod onnx {
    use super::*;
    use crate::recognize::{RecognizeError, Recognizer, RecognizerInfo};
    use ort::session::Session;
    use ort::value::Tensor;

    /// The Penroam handwriting model run with ONNX Runtime on the CPU.
    pub struct OnnxRecognizer {
        bundle: ModelBundle,
        session: Session,
        info: RecognizerInfo,
    }

    fn rt(e: impl std::fmt::Display) -> ModelError {
        ModelError::Runtime(e.to_string())
    }

    impl OnnxRecognizer {
        /// Verifies the bundle and loads its graph (two intra-op threads: recognition runs in
        /// the background and must not take the whole CPU).
        pub fn load(dir: &Path) -> Result<OnnxRecognizer, ModelError> {
            let bundle = ModelBundle::open(dir)?;
            let session = Session::builder()
                .map_err(rt)?
                .with_intra_threads(2)
                .map_err(rt)?
                .commit_from_file(bundle.graph_path())
                .map_err(rt)?;
            let m = &bundle.manifest;
            let info = RecognizerInfo {
                id: "penroam-model".into(),
                model_version: format!("{}@{}", m.id, m.version),
                name: m.name.clone(),
                languages: m.languages.clone(),
                license: m.license.clone(),
            };
            Ok(OnnxRecognizer {
                bundle,
                session,
                info,
            })
        }

        /// The verified bundle.
        pub fn bundle(&self) -> &ModelBundle {
            &self.bundle
        }

        /// Runs the graph on one rendered line and returns `(scores, frames, classes)`.
        pub fn run(&mut self, img: &GrayImage) -> Result<(Vec<f32>, usize, usize), ModelError> {
            let g = &self.bundle.manifest.graph;
            let data = input_tensor(img, g);
            let input = Tensor::from_array((
                [
                    1usize,
                    g.channels as usize,
                    img.height as usize,
                    img.width as usize,
                ],
                data.into_boxed_slice(),
            ))
            .map_err(rt)?;
            let outputs = self
                .session
                .run(ort::inputs![g.input.as_str() => input])
                .map_err(rt)?;
            let (shape, scores) = outputs[g.output.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(rt)?;
            if shape.len() != 3 || shape[0] != 1 {
                return Err(ModelError::Runtime(format!(
                    "expected output [1, frames, classes], got {shape:?}"
                )));
            }
            let (frames, classes) = (shape[1] as usize, shape[2] as usize);
            if classes != self.bundle.manifest.charset.len() {
                return Err(ModelError::Runtime("output classes ≠ charset size".into()));
            }
            Ok((scores.to_vec(), frames, classes))
        }
    }

    impl Recognizer for OnnxRecognizer {
        fn info(&self) -> &RecognizerInfo {
            &self.info
        }

        fn recognize(
            &mut self,
            region: &Region<'_>,
            language: &str,
        ) -> Result<Vec<RecognitionCandidate>, RecognizeError> {
            if !self.info.languages.iter().any(|l| l == language) {
                return Err(RecognizeError::LanguageUnavailable(language.into()));
            }
            let spec = self.bundle.manifest.raster;
            let img = crate::raster::render_region(region, &spec);
            let (scores, frames, classes) = self
                .run(&img)
                .map_err(|e| RecognizeError::Engine(e.to_string()))?;
            let m = &self.bundle.manifest;
            let chars = if m.graph.probabilities {
                let log = log_of_probabilities(&scores);
                ctc_greedy(&log, classes, m.blank, &m.charset, true)
            } else {
                ctc_greedy(&scores, classes, m.blank, &m.charset, m.graph.log_probs)
            };
            Ok(words_on_page(
                &ctc_words(&chars, frames, img.width),
                region,
                &spec,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(s: &str) -> Vec<String> {
        std::iter::once(String::new())
            .chain(s.chars().map(|c| c.to_string()))
            .collect()
    }

    #[test]
    fn greedy_ctc_collapses_repeats_and_drops_blanks() {
        let charset = cs("ab "); // 0 blank, 1 a, 2 b, 3 space
                                 // frames: a a _ a b b _ (space) b
        let best = [1, 1, 0, 1, 2, 2, 0, 3, 2];
        let mut scores = vec![];
        for b in best {
            let mut f = vec![-5.0f32; 4];
            f[b] = -0.1;
            scores.extend(f);
        }
        let out = ctc_greedy(&scores, 4, 0, &charset, true);
        let text: String = out.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "aab b");
        assert_eq!(
            out.iter().map(|c| c.frame).collect::<Vec<_>>(),
            vec![0, 3, 4, 7, 8]
        );
        assert!((out[0].prob - (-0.1f32).exp()).abs() < 1e-6);
        // logits path: softmax probability
        let raw = ctc_greedy(&[0.0, 2.0, 0.0, 0.0], 4, 0, &charset, false);
        assert!((raw[0].prob - 2f32.exp() / (3.0 + 2f32.exp())).abs() < 1e-6);
    }

    fn graph(channels: u32, dark_ink: bool) -> GraphSpec {
        GraphSpec {
            file: "model.onnx".into(),
            input: "x".into(),
            output: "y".into(),
            log_probs: false,
            probabilities: false,
            channels,
            dark_ink,
            mean: 0.5,
            std: 0.5,
        }
    }

    #[test]
    fn input_follows_the_graphs_channels_and_polarity() {
        let img = GrayImage {
            width: 2,
            height: 1,
            pixels: vec![255, 0],
        };
        assert_eq!(input_tensor(&img, &graph(1, false)), vec![1.0, -1.0]);
        assert_eq!(input_tensor(&img, &graph(1, true)), vec![-1.0, 1.0]);
        assert_eq!(
            input_tensor(&img, &graph(3, true)),
            vec![-1.0, 1.0, -1.0, 1.0, -1.0, 1.0],
            "the same plane in every channel, for [1, 3, 1, 2]"
        );
    }

    #[test]
    fn probabilities_decode_with_their_own_confidence() {
        let charset = cs("ab");
        let log = log_of_probabilities(&[0.1, 0.8, 0.1, 0.0, 1.0, 0.0]);
        assert!(log.iter().all(|v| v.is_finite()), "zero stays finite");
        let out = ctc_greedy(&log, 3, 0, &charset, true);
        assert_eq!(out.len(), 1, "the repeat collapses");
        assert_eq!(out[0].text, "a");
        assert!((out[0].prob - 0.8).abs() < 1e-5);
    }

    #[test]
    fn a_manifest_without_the_newer_graph_fields_reads_as_before() {
        let g: GraphSpec = serde_json::from_str(
            r#"{"file": "m", "input": "i", "output": "o", "logProbs": true, "mean": 0.5, "std": 0.5}"#,
        )
        .unwrap();
        assert_eq!((g.channels, g.dark_ink, g.probabilities), (1, false, false));
    }

    #[test]
    fn manifest_is_verified() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("model.onnx"), b"not really onnx").unwrap();
        let (sha, n) = sha256_file(&d.path().join("model.onnx")).unwrap();
        let mut m = Manifest {
            format: FORMAT.into(),
            format_version: 1,
            id: "t".into(),
            version: "1".into(),
            name: "Test".into(),
            license: "test".into(),
            languages: vec!["en".into()],
            decoder: "ctc-greedy".into(),
            raster: RasterSpec::default(),
            graph: GraphSpec {
                file: "model.onnx".into(),
                input: "image".into(),
                output: "log_probs".into(),
                log_probs: true,
                probabilities: false,
                channels: 1,
                dark_ink: false,
                mean: 0.5,
                std: 0.5,
            },
            blank: 0,
            charset: cs("ab"),
            files: [(
                "model.onnx".to_string(),
                FileEntry {
                    sha256: sha,
                    bytes: n,
                },
            )]
            .into(),
            provenance: serde_json::Value::Null,
        };
        let write = |m: &Manifest| {
            std::fs::write(d.path().join("model.json"), serde_json::to_vec(m).unwrap()).unwrap()
        };
        write(&m);
        assert_eq!(
            ModelBundle::open(d.path()).unwrap().manifest.total_bytes(),
            15
        );
        std::fs::write(d.path().join("model.onnx"), b"not really onnX").unwrap();
        assert!(matches!(
            ModelBundle::open(d.path()),
            Err(ModelError::Checksum(_))
        ));
        m.format_version = 2;
        write(&m);
        assert!(matches!(
            ModelBundle::read_manifest(d.path()),
            Err(ModelError::TooNew(2))
        ));
        m.format_version = 1;
        m.graph.channels = 2;
        write(&m);
        assert!(matches!(
            ModelBundle::read_manifest(d.path()),
            Err(ModelError::Manifest(_))
        ));
        m.graph.channels = 1;
        m.files = [(
            "../x".to_string(),
            FileEntry {
                sha256: String::new(),
                bytes: 0,
            },
        )]
        .into();
        m.graph.file = "../x".into();
        write(&m);
        assert!(matches!(
            ModelBundle::read_manifest(d.path()),
            Err(ModelError::Manifest(_))
        ));
    }

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (n, b) in files {
            z.start_file(*n, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    #[test]
    fn archives_install_verified_and_replace_atomically() {
        let d = tempfile::tempdir().unwrap();
        let models = d.path().join("models");
        let onnx = b"graph bytes".as_slice();
        let sha: String = Sha256::digest(onnx)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let manifest = |version: &str, sha: &str| {
            serde_json::json!({
                "format": FORMAT, "formatVersion": 1, "id": "hw-en", "version": version,
                "name": "T", "license": "L", "languages": ["en"], "decoder": "ctc-greedy",
                "raster": RasterSpec::default(),
                "graph": {"file": "model.onnx", "input": "image", "output": "log_probs",
                          "logProbs": true, "mean": 0.5, "std": 0.5},
                "blank": 0, "charset": cs("ab"),
                "files": {"model.onnx": {"sha256": sha, "bytes": onnx.len()}}
            })
            .to_string()
        };
        let good = archive(&[
            ("model.json", manifest("1", &sha).as_bytes()),
            ("model.onnx", onnx),
            ("extra.txt", b"x"),
        ]);
        assert_eq!(inspect_archive(&good).unwrap().version, "1");
        assert!(inspect_archive(b"nope").is_err());
        let b = install_archive(&good, &models).unwrap();
        assert_eq!(b.dir, models.join("hw-en"));
        assert!(!b.dir.join("extra.txt").exists());
        assert_eq!(installed(&models).len(), 1);

        // a corrupt update is refused and leaves version 1 installed
        let bad = archive(&[
            ("model.json", manifest("2", &"0".repeat(64)).as_bytes()),
            ("model.onnx", onnx),
        ]);
        assert!(matches!(
            install_archive(&bad, &models),
            Err(ModelError::Checksum(_))
        ));
        assert_eq!(installed(&models)[0].1.version, "1");
        // path traversal and garbage
        let evil = archive(&[("../model.json", b"{}")]);
        assert!(install_archive(&evil, &models).is_err());
        assert!(install_archive(b"not a zip", &models).is_err());
        // a good update replaces it
        install_archive(
            &archive(&[
                ("model.json", manifest("2", &sha).as_bytes()),
                ("model.onnx", onnx),
            ]),
            &models,
        )
        .unwrap();
        assert_eq!(installed(&models)[0].1.version, "2");
        assert_eq!(
            std::fs::read_dir(&models).unwrap().count(),
            1,
            "no staging leftovers"
        );
    }
}
