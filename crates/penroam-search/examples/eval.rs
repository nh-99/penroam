//! Search and recognition evaluation over a handwriting corpus.
//!
//! ```text
//! cargo run -p penroam-search --release --features windows-ink --example eval -- \
//!     <corpus.json> [--recognizer windows|scripted] [--language NAME] [--device TEXT] [--out report.json]
//! ```
//!
//! The corpus format (written by `tools/search-eval/make-synthetic-corpus.mjs`, or by hand for
//! a real, consented corpus) is described in `docs/development.md` ("Search evaluation"):
//! pages as Penroam page JSON plus the ground truth of each line (text, bounds) and word (text,
//! stroke ids, bounds).
//!
//! Measured, overall and per page style:
//! * segmentation: share of truth lines whose strokes form exactly one region;
//! * CER and WER of the recognized text per truth line (case-insensitive, whitespace
//!   normalised), with the recognized line made of the regions holding its strokes;
//! * search recall@page and recall@region for every truth word of three or more letters
//!   (exact matching, and with fuzzy matching), and false positives for distractor words that
//!   appear on no page;
//! * recognition latency per region and indexing time per page (p50/p95/max), query latency,
//!   and the process's peak memory.
//!
//! `--recognizer scripted` recognizes every truth word perfectly from its stroke ids, which
//! isolates segmentation, indexing and ranking from recognition quality. `model` runs a
//! Penroam handwriting model bundle (`--model`, feature `onnx-model`); `chain` runs the
//! Windows recognizer with that model as fallback, as the app does.
//!
//! Nothing is uploaded; the report is written locally.

use penroam_core::*;
use penroam_search::index::Recognition;
use penroam_search::query::edit_distance;
use penroam_search::*;
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Deserialize)]
struct Corpus {
    pages: Vec<CorpusPage>,
    #[serde(default)]
    distractors: Vec<String>,
    #[serde(default)]
    generator: Option<String>,
}

#[derive(Deserialize)]
struct CorpusPage {
    id: String,
    #[serde(default)]
    style: String,
    page: Page,
    lines: Vec<Line>,
}

#[derive(Deserialize)]
struct Line {
    text: String,
    #[allow(dead_code)]
    bounds: Rect,
    words: Vec<Word>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Word {
    text: String,
    stroke_ids: Vec<Uuid>,
    bounds: Rect,
}

/// Wraps a recognizer to time every call and keep its output per region fingerprint.
struct Timed<'a> {
    inner: &'a mut dyn Recognizer,
    times: Vec<Duration>,
    out: HashMap<String, (Vec<Uuid>, String)>,
}

impl Recognizer for Timed<'_> {
    fn info(&self) -> &RecognizerInfo {
        self.inner.info()
    }

    fn recognize(
        &mut self,
        region: &Region<'_>,
        language: &str,
    ) -> Result<Vec<RecognitionCandidate>, RecognizeError> {
        let t = Instant::now();
        let r = self.inner.recognize(region, language);
        self.times.push(t.elapsed());
        if let Ok(c) = &r {
            let text = c
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            self.out
                .insert(region.fingerprint.clone(), (region.stroke_ids(), text));
        }
        r
    }
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn word_distance(a: &[&str], b: &[&str]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j - 1] + usize::from(a[i - 1] != b[j - 1]))
                .min(prev[j] + 1)
                .min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

fn pct(xs: &mut [f64], p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(f64::total_cmp);
    xs[((xs.len() - 1) as f64 * p).round() as usize]
}

fn ms(d: &[Duration]) -> serde_json::Value {
    let mut v: Vec<f64> = d.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    json!({
        "count": v.len(),
        "p50Ms": pct(&mut v, 0.5),
        "p95Ms": pct(&mut v, 0.95),
        "maxMs": v.iter().cloned().fold(0.0, f64::max),
    })
}

fn peak_memory_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut c = PROCESS_MEMORY_COUNTERS::default();
        // SAFETY: plain out-parameter call on this process's pseudo-handle
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut c,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
            .ok()?;
        }
        Some(c.PeakWorkingSetSize as u64)
    }
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let l = s.lines().find(|l| l.starts_with("VmHWM:"))?;
        let kb: u64 = l.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

#[derive(Default)]
struct Acc {
    lines: usize,
    lines_one_region: usize,
    chars: usize,
    char_errors: usize,
    words: usize,
    word_errors: usize,
    queries: usize,
    page_hits: usize,
    region_hits: usize,
    page_hits_exact: usize,
    region_hits_exact: usize,
}

impl Acc {
    fn report(&self) -> serde_json::Value {
        let r = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
        json!({
            "lines": self.lines,
            "segmentationExact": r(self.lines_one_region, self.lines),
            "cer": r(self.char_errors, self.chars),
            "wer": r(self.word_errors, self.words),
            "queries": self.queries,
            "recallAtPage": r(self.page_hits, self.queries),
            "recallAtRegion": r(self.region_hits, self.queries),
            "recallAtPageExactOnly": r(self.page_hits_exact, self.queries),
            "recallAtRegionExactOnly": r(self.region_hits_exact, self.queries),
        })
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let Some(path) = args.first().filter(|a| !a.starts_with("--")) else {
        eprintln!("usage: eval <corpus.json> [--recognizer windows|scripted] [--language NAME] [--device TEXT] [--out report.json]");
        std::process::exit(2);
    };
    let corpus: Corpus =
        serde_json::from_slice(&std::fs::read(path).expect("read corpus")).expect("parse corpus");
    for p in &corpus.pages {
        penroam_core::validate::validate_page(&p.page)
            .unwrap_or_else(|e| panic!("page {} is invalid: {}", p.id, e.0));
    }
    let which = flag("--recognizer").unwrap_or_else(|| "windows".into());

    let mut scripted = ScriptedRecognizer::new("truth");
    for p in &corpus.pages {
        for l in &p.lines {
            for w in &l.words {
                scripted.word(&w.text, &w.stroke_ids);
            }
        }
    }
    #[cfg(all(windows, feature = "windows-ink"))]
    let mut win = penroam_search::windows_ink::WindowsInkRecognizer::new();
    #[cfg(feature = "onnx-model")]
    let load_model = || {
        let dir = flag("--model").expect("--model <bundle dir> is required");
        penroam_search::model::OnnxRecognizer::load(std::path::Path::new(&dir))
            .unwrap_or_else(|e| panic!("{e}"))
    };
    #[cfg(feature = "onnx-model")]
    let mut model = (which == "model").then(load_model);
    #[cfg(all(feature = "onnx-model", windows, feature = "windows-ink"))]
    let mut chain = (which == "chain").then(|| {
        ChainRecognizer::new(
            penroam_search::windows_ink::WindowsInkRecognizer::new()
                .map(|w| Box::new(w) as Box<dyn Recognizer>),
            Some(Box::new(load_model())),
        )
        .expect("at least the model")
    });
    let rec: &mut dyn Recognizer = match which.as_str() {
        "scripted" => &mut scripted,
        #[cfg(all(windows, feature = "windows-ink"))]
        "windows" => win
            .as_mut()
            .map(|w| w as &mut dyn Recognizer)
            .expect("no Windows handwriting recognizer installed"),
        #[cfg(feature = "onnx-model")]
        "model" => model.as_mut().expect("loaded above"),
        #[cfg(all(feature = "onnx-model", windows, feature = "windows-ink"))]
        "chain" => chain.as_mut().expect("built above"),
        other => {
            eprintln!("recognizer {other:?} is not available in this build");
            std::process::exit(2);
        }
    };
    let language = flag("--language").unwrap_or_else(|| {
        let ls = &rec.info().languages;
        ls.iter()
            .find(|l| l.contains("English (US)") || l.as_str() == "en-US" || l.as_str() == "en")
            .unwrap_or(&ls[0])
            .clone()
    });
    let info = rec.info().clone();

    // one notebook holding every page
    let mut nb = Notebook::new(
        Uuid::from_u128(0xe7a1),
        "Evaluation corpus".into(),
        PageDefaults::default(),
        "#545454".into(),
        0,
    );
    nb.page_order = corpus.pages.iter().map(|p| p.page.id).collect();
    let mut ix = SearchIndex::open_in_memory().expect("index");
    ix.sync_notebook(&nb).expect("notebook");

    let mut timed = Timed {
        inner: rec,
        times: vec![],
        out: HashMap::new(),
    };
    let mut page_times = Vec::new();
    for (i, p) in corpus.pages.iter().enumerate() {
        let t = Instant::now();
        let out = ix
            .index_page(
                nb.id,
                i as u32,
                &p.page,
                Some(Recognition {
                    recognizer: &mut timed,
                    language: &language,
                }),
                &|| false,
            )
            .expect("index page");
        page_times.push(t.elapsed());
        if let Some(e) = out.error {
            eprintln!("page {}: {e}", p.id);
        }
    }
    let region_times = std::mem::take(&mut timed.times);
    let recognized = std::mem::take(&mut timed.out);

    let mut total = Acc::default();
    let mut by_style: BTreeMap<String, Acc> = BTreeMap::new();
    let mut failures: Vec<serde_json::Value> = Vec::new();
    let mut query_times = Vec::new();
    for p in &corpus.pages {
        let regions = penroam_search::segment::regions(&p.page);
        let region_of: HashMap<Uuid, usize> = regions
            .iter()
            .enumerate()
            .flat_map(|(i, r)| r.stroke_ids().into_iter().map(move |s| (s, i)))
            .collect();
        let acc = by_style.entry(p.style.clone()).or_default();
        let page_words: HashSet<String> = p
            .lines
            .iter()
            .flat_map(|l| l.words.iter().map(|w| w.text.to_lowercase()))
            .collect();
        let mut queried: HashSet<String> = HashSet::new();
        for l in &p.lines {
            let strokes: Vec<Uuid> = l.words.iter().flat_map(|w| w.stroke_ids.clone()).collect();
            let rs: Vec<usize> = {
                let mut v: Vec<usize> = strokes
                    .iter()
                    .filter_map(|s| region_of.get(s))
                    .copied()
                    .collect();
                v.sort();
                v.dedup();
                v
            };
            let one = rs.len() == 1 && regions[rs[0]].strokes.len() == strokes.len();
            // recognized text of the regions holding this line, left to right
            let mut parts: Vec<(f64, String)> = rs
                .iter()
                .filter_map(|i| {
                    recognized
                        .get(&regions[*i].fingerprint)
                        .map(|(_, t)| (regions[*i].bounds.x, t.clone()))
                })
                .collect();
            parts.sort_by(|a, b| a.0.total_cmp(&b.0));
            let got = norm(&parts.into_iter().map(|p| p.1).collect::<Vec<_>>().join(" "));
            let want = norm(&l.text);
            let ce = edit_distance(&got, &want);
            let gw: Vec<&str> = got.split(' ').filter(|s| !s.is_empty()).collect();
            let ww: Vec<&str> = want.split(' ').collect();
            let we = word_distance(&gw, &ww);
            for a in [&mut total, &mut *acc] {
                a.lines += 1;
                a.lines_one_region += one as usize;
                a.chars += want.chars().count();
                a.char_errors += ce;
                a.words += ww.len();
                a.word_errors += we;
            }
            if we > 0 && failures.len() < 40 {
                failures.push(json!({ "page": p.id, "style": p.style, "truth": want, "recognized": got, "oneRegion": one }));
            }
            for w in &l.words {
                let q = w.text.to_lowercase();
                if q.chars().count() < 3 || !queried.insert(q.clone()) {
                    continue;
                }
                let t = Instant::now();
                let hits = ix.search(&q, 50).expect("search");
                query_times.push(t.elapsed());
                let on_page = |h: &&SearchHit| h.page_id == Some(p.page.id);
                let at_region = |h: &&SearchHit| {
                    on_page(h)
                        && (if h.match_bounds.is_empty() {
                            h.bounds.iter().collect::<Vec<_>>()
                        } else {
                            h.match_bounds.iter().collect()
                        })
                        .into_iter()
                        .any(|b| overlaps(b, &w.bounds))
                };
                let exact: Vec<&SearchHit> = hits.iter().filter(|h| !h.fuzzy).collect();
                for a in [&mut total, &mut *acc] {
                    a.queries += 1;
                    a.page_hits += hits.iter().any(|h| on_page(&h)) as usize;
                    a.region_hits += hits.iter().any(|h| at_region(&h)) as usize;
                    a.page_hits_exact += exact.iter().any(on_page) as usize;
                    a.region_hits_exact += exact.iter().any(at_region) as usize;
                }
            }
        }
        let _ = page_words;
    }
    // false positives: distractor words appear on no page
    let mut fp_queries = 0;
    let mut fp_hits = 0;
    let mut fp_exact = 0;
    for d in &corpus.distractors {
        let hits = ix.search(d, 50).expect("search");
        fp_queries += 1;
        fp_hits += !hits.is_empty() as usize;
        fp_exact += hits.iter().any(|h| !h.fuzzy) as usize;
    }

    let report = json!({
        "corpus": path,
        "generator": corpus.generator,
        "device": flag("--device"),
        "os": std::env::consts::OS,
        "recognizer": info,
        "language": language,
        "pages": corpus.pages.len(),
        "overall": total.report(),
        "byStyle": by_style.iter().map(|(k, v)| (k.clone(), v.report())).collect::<serde_json::Map<_, _>>(),
        "falsePositives": {
            "distractorQueries": fp_queries,
            "queriesWithAnyHit": fp_hits,
            "queriesWithExactHit": fp_exact,
        },
        "latency": {
            "recognitionPerRegion": ms(&region_times),
            "indexPerPage": ms(&page_times),
            "query": ms(&query_times),
        },
        "peakMemoryBytes": peak_memory_bytes(),
        "sampleFailures": failures,
    });
    let text = serde_json::to_string_pretty(&report).expect("report");
    if let Some(out) = flag("--out") {
        std::fs::write(&out, &text).expect("write report");
    }
    println!("{text}");
}
