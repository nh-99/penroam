# Shared fixtures

| File | What |
|---|---|
| `generate-fixtures.mjs` | Reference implementation of the PRNG, scene generator, width mapping and stroke geometry. Regenerate with `node fixtures/generate-fixtures.mjs`, verify with `--check` |
| `sample-strokes.json` | Hand-authored strokes: dot, line, rising-pressure curve, loop, constant-width mouse stroke |
| `geometry-golden.json` | Expected `widthFor` / `strokePieces` output. TS, Kotlin and Dart tests compare against it |
| `benchmark-scenes/scene-{100,1000,5000,10000}.json` | Workload D scenes (1600×1200 doc units). The apps regenerate them in-process with ported generators; tests prove they are identical to these files |
| `manifest.json` | sha256 and size of every generated file |
