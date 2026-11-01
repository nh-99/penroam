# Flutter prototype

`Listener` + `CustomPainter` (Mode A) with separate RepaintBoundaries for committed and active
ink, and `ui.Picture` chunk caching. On Android, the shared `BooxInkView` is hosted through
Platform Views (Modes B/C). Hybrid Composition, TLHC and AndroidView are selectable at runtime.

```bash
tool/bootstrap.sh                 # flutter create (android/ios/linux runners) + wire android/boox-ink
flutter test                      # geometry golden, scene determinism, widget test
flutter run -d linux --release
M00_AUTOBENCH=1 M00_RESULTS_DIR=$PWD/../benchmarks/results/linux build/linux/x64/release/bundle/m00_flutter_app
flutter build apk --release       # BOOX
flutter run -d <ipad> --profile   # iPad (macOS)
```

The hand-written parts of `android/` are `app/src/main/kotlin/dev/opennote/m00/m00_flutter_app/{MainActivity,BooxPlatformView}.kt`.
`flutter create` generates the rest, and `tool/patch_android.py` patches it.

**Status:** this Dart code has not been compiled yet. The Flutter SDK was not available in the
build session. The Android Kotlin has been compiled against the Flutter embedding artifact.
