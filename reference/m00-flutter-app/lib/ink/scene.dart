// Deterministic benchmark scenes. Must reproduce
// fixtures/benchmark-scenes/scene-<N>.json exactly (test/scene_test.dart).
//
// mulberry32 is ported with explicit 32-bit masking. Dart VM/AOT ints are
// 64-bit two's complement, so (a * b) & 0xFFFFFFFF equals JS Math.imul
// modulo 2^32. (Flutter web is not a target in M00.)
import 'model.dart';

const int _m32 = 0xFFFFFFFF;

int _imul(int a, int b) => (a * b) & _m32;

double Function() mulberry32(int seed) {
  var a = seed & _m32;
  return () {
    a = (a + 0x6D2B79F5) & _m32;
    var t = a;
    t = _imul(t ^ (t >> 15), t | 1);
    t = (t ^ ((t + _imul(t ^ (t >> 7), t | 61)) & _m32)) & _m32;
    return ((t ^ (t >> 14)) & _m32) / 4294967296.0;
  };
}

const double sceneWidth = 1600;
const double sceneHeight = 1200;
const List<double> _widths = [1.5, 3.0, 6.0];

// JS Math.round rounds .5 up; Dart's round() rounds half away from zero.
// All rounded values here are >= 0, where the two agree.
double _round2(double v) => (v * 100).round() / 100;
double _round3(double v) => (v * 1000).round() / 1000;
double _clamp(double v, double lo, double hi) => v < lo ? lo : (v > hi ? hi : v);

List<InkStroke> generateScene(int count) {
  final rnd = mulberry32(count);
  final strokes = <InkStroke>[];
  for (var i = 0; i < count; i++) {
    var x = 40 + rnd() * (sceneWidth - 80);
    var y = 40 + rnd() * (sceneHeight - 80);
    final n = 8 + (rnd() * 40).floor();
    var vx = (rnd() - 0.5) * 6;
    var vy = (rnd() - 0.5) * 6;
    var p = 0.3 + 0.5 * rnd();
    final width = _widths[(rnd() * 3).floor()];
    final points = <InkPoint>[];
    for (var j = 0; j < n; j++) {
      if (j > 0) {
        vx = vx * 0.85 + (rnd() - 0.5) * 3;
        vy = vy * 0.85 + (rnd() - 0.5) * 3;
        x = _clamp(x + vx, 0, sceneWidth);
        y = _clamp(y + vy, 0, sceneHeight);
        p = p * 0.7 + 0.3 * (0.2 + 0.7 * rnd());
      }
      points.add(InkPoint(_round2(x), _round2(y), _round3(p), timeUs: j * 4000));
    }
    strokes.add(InkStroke(
      id: 'scene$count-$i',
      source: 'fixture',
      input: {'api': 'generated', 'device': 'none'},
      brush: InkBrush(width: width),
      points: points,
    ));
  }
  return strokes;
}
