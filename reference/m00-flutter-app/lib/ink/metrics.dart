// Benchmark recorder. Names follow benchmarks/protocol.md and match the
// TypeScript recorder (tauri_app/src/ink/metrics.ts).
//
// Clock: a process-wide Stopwatch (monotonic). PointerEvent.timeStamp comes
// from the engine/platform with an unspecified epoch, so it is only used for
// intra-stroke sample spacing, never subtracted from the Stopwatch.
import 'dart:collection';
import 'dart:math' as math;

final Stopwatch monotonic = Stopwatch()..start();
double nowMs() => monotonic.elapsedMicroseconds / 1000.0;

Map<String, dynamic> summarize(List<double> values, [int? total]) {
  final out = <String, dynamic>{'count': total ?? values.length, 'retained': values.length};
  if (values.isEmpty) return out;
  final sorted = [...values]..sort();
  double pct(double p) => sorted[((sorted.length - 1) * p).round()];
  out['median'] = pct(0.5);
  out['p95'] = pct(0.95);
  out['max'] = sorted.last;
  out['min'] = sorted.first;
  out['mean'] = sorted.reduce((a, b) => a + b) / sorted.length;
  return out;
}

class _Series {
  _Series(this.cap);
  final int cap;
  final List<double> values = [];
  int count = 0;
  int next = 0;
  void add(double v) {
    if (values.length < cap) {
      values.add(v);
    } else {
      values[next] = v;
    }
    next = (next + 1) % cap;
    count++;
  }
}

class Metrics {
  Metrics([this.cap = 8192]);
  final int cap;
  final Map<String, int> counters = {};
  final Map<String, _Series> _series = {};
  final Queue<Map<String, dynamic>> events = Queue();
  final List<Map<String, dynamic>> strokes = [];
  final List<String> errors = [];
  final Map<String, dynamic> info = {};

  void count(String name, [int d = 1]) => counters[name] = (counters[name] ?? 0) + d;
  void timing(String name, double ms) => _series.putIfAbsent(name, () => _Series(cap)).add(ms);
  void since(String name, double t0) => timing(name, nowMs() - t0);

  void event(String name, [Map<String, dynamic> detail = const {}]) {
    count(name);
    events.add({'name': name, 't': nowMs(), ...detail});
    while (events.length > 512) {
      events.removeFirst();
    }
  }

  void error(String msg) {
    errors.add(msg);
    if (errors.length > 64) errors.removeAt(0);
  }

  void stroke(Map<String, dynamic> s) {
    strokes.add(s);
    if (strokes.length > 512) strokes.removeAt(0);
  }

  Map<String, dynamic>? get(String name) {
    final s = _series[name];
    return s == null ? null : summarize(s.values, s.count);
  }

  void reset() {
    counters.clear();
    _series.clear();
    events.clear();
    strokes.clear();
    errors.clear();
  }

  Map<String, dynamic> toJson() => {
        'clock': 'Stopwatch (monotonic)',
        'counters': counters,
        'timings': {for (final e in _series.entries) e.key: summarize(e.value.values, e.value.count)},
        'events': events.toList(),
        'strokes': strokes,
        'errors': errors,
        'info': info,
      };
}

/// Gap analysis on delivered sample timestamps (same rule as Kotlin/TS).
Map<String, dynamic> analyzeSamples(List<double> ts, List<double> xs, List<double> ys) {
  final dts = <double>[for (var i = 1; i < ts.length; i++) ts[i] - ts[i - 1]];
  final sorted = [...dts]..sort();
  final median = sorted.isEmpty ? 0.0 : sorted[sorted.length ~/ 2];
  final gapThreshold = math.max(30.0, median * 4);
  var gaps = 0, jumps = 0;
  for (var i = 0; i < dts.length; i++) {
    if (dts[i] > gapThreshold) gaps++;
    final dx = xs[i + 1] - xs[i], dy = ys[i + 1] - ys[i];
    if (math.sqrt(dx * dx + dy * dy) > 40) jumps++;
  }
  return {'medianDtMs': median, 'maxDtMs': sorted.isEmpty ? 0.0 : sorted.last, 'timeGaps': gaps, 'spatialJumps': jumps};
}
