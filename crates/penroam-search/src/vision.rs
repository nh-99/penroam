//! Apple Vision text recognition (macOS, iOS / iPadOS) as a handwriting provider.
//!
//! Vision recognizes text in images, so each region is rendered with [`crate::raster`] at a
//! larger size than the Penroam model uses (Vision is tuned for photographed text), inverted
//! to dark ink on white, wrapped in a grayscale `CGImage` and passed to a
//! `VNRecognizeTextRequest` at the accurate level with language correction. Vision returns
//! line observations with candidate strings; each line's words are located proportionally
//! along its bounding box and mapped back to strokes with [`crate::model::words_on_page`].
//!
//! Everything runs on the device inside the system framework; nothing is downloaded.
//! Vision objects are not `Send`, so they are created per call inside an autorelease pool.
//!
//! **Status:** written and reviewed against the `objc2-vision` bindings, but not yet built or
//! run on Apple hardware (development so far happened on Windows). Handwriting support in
//! Vision varies by OS version and language.

use crate::model::{words_on_page, ImageWord};
use crate::raster::{render_region, GrayImage, RasterSpec};
use crate::recognize::{RecognitionCandidate, RecognizeError, Recognizer, RecognizerInfo};
use crate::segment::Region;
use objc2::rc::autoreleasepool;
use objc2::AnyThread;
use objc2_core_foundation::CFData;
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};

/// Rendering for Vision: tall lines with thick strokes.
const VISION_RASTER: RasterSpec = RasterSpec {
    height: 96,
    padding: 16,
    max_width: 4096,
    stroke_width: 4.0,
};

/// Apple Vision's text recognizer.
pub struct VisionRecognizer {
    info: RecognizerInfo,
}

impl VisionRecognizer {
    /// The provider, or `None` when Vision reports no recognition languages.
    pub fn new() -> Option<VisionRecognizer> {
        let languages = autoreleasepool(|_| {
            let req = VNRecognizeTextRequest::new();
            req.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
            // SAFETY: plain query on a fresh request
            unsafe { req.supportedRecognitionLanguagesAndReturnError() }
                .map(|a| a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap_or_default()
        });
        if languages.is_empty() {
            return None;
        }
        Some(VisionRecognizer {
            info: RecognizerInfo {
                id: "apple-vision".into(),
                model_version: "1".into(),
                name: "Apple Vision text recognition".into(),
                languages,
                license: "Part of the operating system; runs on this device".into(),
            },
        })
    }
}

fn cg_image(img: &GrayImage) -> Option<objc2_core_foundation::CFRetained<CGImage>> {
    // dark ink on white, as in a photographed page
    let bytes: Vec<u8> = img.pixels.iter().map(|p| 255 - p).collect();
    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let space = CGColorSpace::new_device_gray()?;
    // SAFETY: the provider holds width × height 8-bit gray bytes, rows tightly packed
    unsafe {
        CGImage::new(
            img.width as usize,
            img.height as usize,
            8,
            8,
            img.width as usize,
            Some(&space),
            CGBitmapInfo(CGImageAlphaInfo::None.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
}

impl Recognizer for VisionRecognizer {
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
        let img = render_region(region, &VISION_RASTER);
        let words = autoreleasepool(|_| -> Result<Vec<ImageWord>, RecognizeError> {
            let cg = cg_image(&img)
                .ok_or_else(|| RecognizeError::Engine("could not create the image".into()))?;
            let req = VNRecognizeTextRequest::new();
            req.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
            req.setUsesLanguageCorrection(true);
            req.setRecognitionLanguages(&NSArray::from_retained_slice(&[NSString::from_str(
                language,
            )]));
            let options = NSDictionary::new();
            // SAFETY: an empty options dictionary is valid for initWithCGImage:options:
            let handler = unsafe {
                VNImageRequestHandler::initWithCGImage_options(
                    VNImageRequestHandler::alloc(),
                    &cg,
                    &options,
                )
            };
            let as_request: &VNRequest = &req;
            handler
                .performRequests_error(&NSArray::from_slice(&[as_request]))
                .map_err(|e| RecognizeError::Engine(e.localizedDescription().to_string()))?;
            let mut words = Vec::new();
            let mut lines: Vec<(f64, f64, Vec<String>, f32)> = Vec::new();
            for obs in req.results().iter().flat_map(|r| r.iter()) {
                let cands = obs.topCandidates(3);
                let texts: Vec<String> = cands.iter().map(|c| c.string().to_string()).collect();
                let Some(best) = cands.iter().next() else {
                    continue;
                };
                // normalised, origin bottom-left; only x matters for word placement
                // SAFETY: plain property read
                let bb = unsafe { obs.boundingBox() };
                let (l, r) = (
                    bb.origin.x * img.width as f64,
                    (bb.origin.x + bb.size.width) * img.width as f64,
                );
                lines.push((l, r, texts, best.confidence()));
            }
            lines.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (l, r, texts, conf) in lines {
                let best = &texts[0];
                let chars = best.chars().count().max(1) as f64;
                // other readings of the whole line stay searchable, kept once on its first word
                let mut alternates: Vec<String> = texts[1..].to_vec();
                // place each word proportionally to its character offsets in the line
                let mut offset = 0usize;
                for w in best.split(' ') {
                    let n = w.chars().count();
                    if n > 0 {
                        let a = l + (r - l) * offset as f64 / chars;
                        let b = l + (r - l) * (offset + n) as f64 / chars;
                        words.push(ImageWord {
                            text: w.to_string(),
                            alternates: std::mem::take(&mut alternates),
                            confidence: Some(conf),
                            left: a,
                            right: b,
                        });
                    }
                    offset += n + 1;
                }
            }
            Ok(words)
        })?;
        Ok(words_on_page(&words, region, &VISION_RASTER))
    }
}
