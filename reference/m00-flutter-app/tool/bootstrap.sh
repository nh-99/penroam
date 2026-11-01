#!/usr/bin/env bash
# Generates the platform runners (android/ios/linux) that are not committed
# by hand, then wires in the shared BOOX module. Safe to re-run:
# `flutter create` never overwrites existing files (our MainActivity.kt and
# BooxPlatformView.kt are kept), and patch_android.py is idempotent.
set -euo pipefail
cd "$(dirname "$0")/.."
flutter --version
flutter create --platforms=android,ios,linux --org dev.opennote.m00 --project-name m00_flutter_app .
# flutter create adds a counter-app test; ours live in test/*_test.dart.
rm -f test/widget_test.dart
python3 tool/patch_android.py
flutter pub get
echo "bootstrap done. Try: flutter test && flutter run -d linux --release"
