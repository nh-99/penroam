// Canonical M00 stroke format (docs/architecture.md). Identical to the
// TypeScript (tauri_app/src/ink/model.ts) and Kotlin (android/boox-ink
// InkModel.kt) representations.
import 'dart:convert';
import 'dart:math' as math;

class InkPoint {
  const InkPoint(this.x, this.y, this.pressure, {this.tiltX = 0, this.tiltY = 0, this.timeUs = 0});

  final double x; // document units (logical px at zoom 1)
  final double y;
  final double pressure; // normalized 0..1 (mouse: 0.5)
  final double tiltX; // degrees, W3C convention
  final double tiltY;
  final int timeUs; // microseconds since first sample of the stroke

  Map<String, dynamic> toJson() =>
      {'x': x, 'y': y, 'pressure': pressure, 'tiltX': tiltX, 'tiltY': tiltY, 'timeUs': timeUs};

  factory InkPoint.fromJson(Map<String, dynamic> j) => InkPoint(
        (j['x'] as num).toDouble(),
        (j['y'] as num).toDouble(),
        ((j['pressure'] ?? 0.5) as num).toDouble(),
        tiltX: ((j['tiltX'] ?? 0) as num).toDouble(),
        tiltY: ((j['tiltY'] ?? 0) as num).toDouble(),
        timeUs: ((j['timeUs'] ?? 0) as num).toInt(),
      );

  @override
  bool operator ==(Object other) =>
      other is InkPoint &&
      other.x == x &&
      other.y == y &&
      other.pressure == pressure &&
      other.tiltX == tiltX &&
      other.tiltY == tiltY &&
      other.timeUs == timeUs;

  @override
  int get hashCode => Object.hash(x, y, pressure, tiltX, tiltY, timeUs);
}

class InkBrush {
  const InkBrush({this.type = 'pen', this.width = 3, this.color = '#000000', this.pressure = true});
  final String type;
  final double width;
  final String color;
  final bool pressure;

  Map<String, dynamic> toJson() => {'type': type, 'width': width, 'color': color, 'pressure': pressure};

  factory InkBrush.fromJson(Map<String, dynamic>? j) => InkBrush(
        type: (j?['type'] ?? 'pen') as String,
        width: ((j?['width'] ?? 3) as num).toDouble(),
        color: (j?['color'] ?? '#000000') as String,
        pressure: j?['pressure'] != false,
      );
}

class InkStroke {
  InkStroke({required this.id, required this.source, required this.input, required this.brush, required this.points});

  final String id;
  final String source; // stylus | mouse | touch | fixture
  /// {api: flutter-pointer | onyx-touchhelper | android-motionevent | generated, device: ..., ...diagnostics}
  final Map<String, dynamic> input;
  final InkBrush brush;
  final List<InkPoint> points;

  List<double>? _bounds;

  Map<String, dynamic> toJson() => {
        'id': id,
        'source': source,
        'input': input,
        'brush': brush.toJson(),
        'points': [for (final p in points) p.toJson()],
      };

  factory InkStroke.fromJson(Map<String, dynamic> j) => InkStroke(
        id: (j['id'] ?? newId()) as String,
        source: (j['source'] ?? 'unknown') as String,
        input: {'api': 'unknown', 'device': 'unknown', ...?(j['input'] as Map?)?.cast<String, dynamic>()},
        brush: InkBrush.fromJson((j['brush'] as Map?)?.cast<String, dynamic>()),
        points: [for (final p in (j['points'] as List? ?? const [])) InkPoint.fromJson((p as Map).cast<String, dynamic>())],
      );

  /// Cached [minX, minY, maxX, maxY] including half the max width.
  List<double> get bounds => _bounds ??= _computeBounds();

  List<double> _computeBounds() {
    var minX = double.infinity, minY = double.infinity, maxX = -double.infinity, maxY = -double.infinity, maxW = 0.0;
    for (final p in points) {
      minX = math.min(minX, p.x);
      minY = math.min(minY, p.y);
      maxX = math.max(maxX, p.x);
      maxY = math.max(maxY, p.y);
      final w = widthForInternal(brush.width, p.pressure, brush.pressure);
      if (w > maxW) maxW = w;
    }
    final pad = maxW / 2 + 1;
    return [minX - pad, minY - pad, maxX + pad, maxY + pad];
  }
}

// Duplicated from geometry.dart to avoid an import cycle.
double widthForInternal(double base, double pressure, bool enabled) {
  if (!enabled) return base;
  final p = pressure < 0 ? 0.0 : (pressure > 1 ? 1.0 : pressure);
  return math.max(0.5, base * (0.4 + 1.2 * p));
}

final _rnd = math.Random();
String newId() {
  String h(int n) => List.generate(n, (_) => _rnd.nextInt(16).toRadixString(16)).join();
  return '${h(8)}-${h(4)}-4${h(3)}-${h(4)}-${h(12)}';
}

Map<String, dynamic> toDocument(List<InkStroke> strokes, [Map<String, dynamic> meta = const {}]) => {
      'format': 'm00-strokes',
      'version': 1,
      'meta': meta,
      'strokes': [for (final s in strokes) s.toJson()],
    };

List<InkStroke> parseDocument(String text) {
  final raw = jsonDecode(text);
  final List list = raw is List ? raw : ((raw as Map)['strokes'] as List? ?? const []);
  return [for (final s in list) InkStroke.fromJson((s as Map).cast<String, dynamic>())];
}
