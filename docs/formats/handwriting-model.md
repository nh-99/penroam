# Handwriting model bundle, format 1 (normative)

Status: implemented contract. Reference implementation: `crates/penroam-search/src/model.rs`
(verification, install, CTC decoding, ONNX Runtime inference) and `src/raster.rs` (input
rendering).

The Penroam handwriting model is an optional, image-based line recognizer. Search uses it where
the platform has no handwriting recognizer, and as a fallback for lines the platform recognizer
fails on (`ChainRecognizer`). It is never required: without it, titles, typed text and PDF text
are still searchable. The app does not include a model. Devices get one from a signed catalog
(§12c of [`../protocol/sync-protocol-v1.md`](../protocol/sync-protocol-v1.md)) or from a file the
user picks.

## Files

A bundle is a directory:

```text
model.json     the manifest (below)
model.onnx     the graph (any name; listed in model.json)
LICENSE        any other files the model's licence requires (listed in model.json)
```

For installing it is packed as a `.penroam-model` file: a zip with those files at the top level
(no folders). Install (`install_archive`, or `install_archive_from` for a file on disk) extracts to a staging directory, refuses
names with `/`, `\`, `:` or a leading `.`, caps the total at 512 MiB, verifies every file's size
and SHA-256 against the manifest, drops unlisted files, and only then replaces the installed
bundle `<app data>/models/handwriting/<id>/`. A failed install leaves the previous model in
place. One model is installed at a time.

The checksums in `model.json` come with the bundle, so they catch damage, not substitution. For a
download, the signed catalog's SHA-256 of the whole archive provides that assurance. For a
file, the user chooses what to trust and sees the name, version, size and licence first.

## `model.json`

```json
{
  "format": "penroam-handwriting-model",
  "formatVersion": 1,
  "id": "penroam-handwriting-en",
  "version": "2026.11.0",
  "name": "Penroam handwriting model (English)",
  "license": "…shown to the user before installing…",
  "languages": ["en"],
  "decoder": "ctc-greedy",
  "raster": { "height": 40, "padding": 4, "maxWidth": 1600, "strokeWidth": 2.0 },
  "graph": { "file": "model.onnx", "input": "image", "output": "log_probs",
             "logProbs": true, "probabilities": false, "channels": 1, "darkInk": false,
             "mean": 0.5, "std": 0.5 },
  "blank": 0,
  "charset": ["", " ", "!", "…"],
  "files": { "model.onnx": { "sha256": "<lowercase hex>", "bytes": 12066358 } },
  "provenance": { "…": "free-form, shown, never interpreted" }
}
```

Rules:

* `format` must equal `penroam-handwriting-model`; a `formatVersion` above 1 is refused
  ("newer than this version of Penroam supports"); `decoder` must be `ctc-greedy`.
* `id` is `[A-Za-z0-9_-]+`; `version` is opaque. The search index stores
  `penroam-model/<id>@<version>`, so a new version re-recognizes the library in the background.
* `blank` must index into `charset`; the graph's class count must equal `charset.length`.
* `probabilities`, `channels` and `darkInk` are optional, defaulting to `false`, `1` and `false`.
  `channels` is 1 or 3.
* `files` lists every file except `model.json`, by plain name.

## Inference contract

1. Render the region with [`raster.rs`](../../crates/penroam-search/src/raster.rs) using the
   manifest's `raster` spec: sample bounds scaled to the height, strokes of fixed width,
   coverage 0–255 (255 = ink). Training must reproduce this exactly; the training pipeline keeps
   a byte-exact port checked against golden images.
2. Input tensor `graph.input`: `float32 [1, channels, height, width]`, value
   `(v / 255 − mean) / std`, where `v` is the coverage, or `255 − coverage` when `darkInk` is
   true. With 3 channels each holds the same plane.
3. Output tensor `graph.output`: `float32 [1, frames, classes]`: probabilities when
   `probabilities` is true, otherwise log-probabilities when `logProbs` is true, otherwise
   logits (softmax is applied for confidences).
4. Greedy CTC: best class per frame, repeats collapsed, `blank` dropped, classes mapped through
   `charset`. A character's confidence is its probability at the frame it was emitted.
5. Words are split on whitespace. Frame `t` covers image x `[t, t + 1) · width / frames`; a
   word's x range maps back to page points through the rasterizer's scale, and each stroke goes
   to the word whose span holds its horizontal centre (`model::words_on_page`).

## Runtime

ONNX Runtime on the CPU with two intra-op threads, on the search worker's background thread.
Desktop and iOS builds link pyke's prebuilt ONNX Runtime (`ort`, `download-binaries`); Android
loads Microsoft's `libonnxruntime.so` from the `onnxruntime-android` package at run time
(`load-dynamic`).

The runtime is part of the app; only the model is downloaded. Apps cannot be updated with a new
graph that needs a newer runtime than they contain, so a catalog entry's `min_app_version` must
name the first app version whose runtime can run it.

## Publishing

Penroam Cloud publishes the model promoted in OtterML (private): it builds the bundle from the
run, signs a catalog and offers it to clients, without an app release. The run itself supplies
everything in `model.json` that describes the model.

`crates/penroam-search/examples/model_catalog.rs` makes the signing key, and builds and signs a
catalog from archives for servers that publish by hand, taking each entry's id, version, name,
licence and languages from the archive. See
[`../development.md`](../development.md#handwriting-model-catalog).

## Training

A model comes from outside this repository; only the resulting bundle reaches users, and its
provenance says where it came from. A model trained with OtterML (private) must use training
data that is licence-clean for commercial use (no non-commercial handwriting datasets). A
third-party model's weights must carry a licence that allows redistribution, and its licence
text ships in the bundle.
