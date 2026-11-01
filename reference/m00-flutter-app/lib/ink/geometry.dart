// Dart port of the shared brush geometry. Reference implementation and spec:
// fixtures/generate-fixtures.mjs. Verified by test/geometry_test.dart.
import 'dart:math' as math;
import 'dart:ui';

import 'model.dart';

enum RenderStyle { smooth, polyline }

double widthFor(double base, double pressure, bool pressureEnabled) => widthForInternal(base, pressure, pressureEnabled);

sealed class Piece {
  const Piece(this.width);
  final double width;
}

class DotPiece extends Piece {
  const DotPiece(this.x, this.y, double width) : super(width);
  final double x, y;
}

class LinePiece extends Piece {
  const LinePiece(this.x0, this.y0, this.x1, this.y1, double width) : super(width);
  final double x0, y0, x1, y1;
}

class QuadPiece extends Piece {
  const QuadPiece(this.x0, this.y0, this.cx, this.cy, this.x1, this.y1, double width) : super(width);
  final double x0, y0, cx, cy, x1, y1;
}

int pieceCount(int n) => n == 0 ? 0 : (n <= 2 ? 1 : n);

/// Pieces that cannot change once n points are known (all but the trailing line).
int stablePieceCount(int n) => n < 3 ? 0 : n - 1;

Piece piece(List<InkPoint> pts, int k, double base, bool pe) {
  final n = pts.length;
  double w(InkPoint p) => widthFor(base, p.pressure, pe);
  if (n == 1) return DotPiece(pts[0].x, pts[0].y, w(pts[0]));
  if (n == 2) return LinePiece(pts[0].x, pts[0].y, pts[1].x, pts[1].y, w(pts[0]));
  if (k == 0) {
    final a = pts[0], b = pts[1];
    return LinePiece(a.x, a.y, (a.x + b.x) / 2, (a.y + b.y) / 2, w(a));
  }
  if (k == n - 1) {
    final a = pts[n - 2], b = pts[n - 1];
    return LinePiece((a.x + b.x) / 2, (a.y + b.y) / 2, b.x, b.y, w(b));
  }
  final p0 = pts[k - 1], p1 = pts[k], p2 = pts[k + 1];
  return QuadPiece((p0.x + p1.x) / 2, (p0.y + p1.y) / 2, p1.x, p1.y, (p1.x + p2.x) / 2, (p1.y + p2.y) / 2, w(p1));
}

List<Piece> strokePieces(List<InkPoint> pts, double base, bool pe) =>
    [for (var k = 0; k < pieceCount(pts.length); k++) piece(pts, k, base, pe)];

final Paint _stroke = Paint()
  ..color = const Color(0xFF000000)
  ..style = PaintingStyle.stroke
  ..strokeCap = StrokeCap.round
  ..strokeJoin = StrokeJoin.round
  ..isAntiAlias = true;

final Paint _fill = Paint()
  ..color = const Color(0xFF000000)
  ..style = PaintingStyle.fill
  ..isAntiAlias = true;

/// Draws pieces [from, to) of [s] onto [c]; c must carry the doc transform.
void drawPieces(Canvas c, InkStroke s, int from, int to, [RenderStyle style = RenderStyle.smooth, double opacity = 1]) {
  final pts = s.points;
  final n = pts.length;
  if (n == 0 || from >= to) return;
  final base = s.brush.width;
  final alpha = (opacity * 255).round();
  _stroke.color = Color.fromARGB(alpha, 0, 0, 0);
  _fill.color = _stroke.color;
  if (style == RenderStyle.polyline) {
    _stroke.strokeWidth = base;
    if (n == 1) {
      c.drawCircle(Offset(pts[0].x, pts[0].y), base / 2, _fill);
      return;
    }
    final start = math.max(0, from - 1);
    final end = math.min(n - 1, to);
    final path = Path()..moveTo(pts[start].x, pts[start].y);
    for (var i = start + 1; i <= end; i++) {
      path.lineTo(pts[i].x, pts[i].y);
    }
    c.drawPath(path, _stroke);
    return;
  }
  if (!s.brush.pressure && n >= 2) {
    _stroke.strokeWidth = base;
    final path = Path();
    for (var k = from; k < to; k++) {
      final pc = piece(pts, k, base, false);
      if (pc is LinePiece) {
        if (k == from) path.moveTo(pc.x0, pc.y0);
        path.lineTo(pc.x1, pc.y1);
      } else if (pc is QuadPiece) {
        if (k == from) path.moveTo(pc.x0, pc.y0);
        path.quadraticBezierTo(pc.cx, pc.cy, pc.x1, pc.y1);
      }
    }
    c.drawPath(path, _stroke);
    return;
  }
  for (var k = from; k < to; k++) {
    drawPiece(c, piece(pts, k, base, s.brush.pressure));
  }
}

void drawStroke(Canvas c, InkStroke s, [RenderStyle style = RenderStyle.smooth]) =>
    drawPieces(c, s, 0, pieceCount(s.points.length), style);

void drawPiece(Canvas c, Piece pc) {
  switch (pc) {
    case DotPiece():
      c.drawCircle(Offset(pc.x, pc.y), pc.width / 2, _fill);
    case LinePiece():
      _stroke.strokeWidth = pc.width;
      c.drawLine(Offset(pc.x0, pc.y0), Offset(pc.x1, pc.y1), _stroke);
    case QuadPiece():
      _stroke.strokeWidth = pc.width;
      c.drawPath(
        Path()
          ..moveTo(pc.x0, pc.y0)
          ..quadraticBezierTo(pc.cx, pc.cy, pc.x1, pc.y1),
        _stroke,
      );
  }
}

/// Stroke eraser hit test (document units). Same rule as Kotlin/TypeScript.
bool hitTest(InkStroke s, double cx, double cy, double r) {
  final b = s.bounds;
  if (cx < b[0] - r || cx > b[2] + r || cy < b[1] - r || cy > b[3] + r) return false;
  final pts = s.points;
  final half = s.brush.width / 2;
  if (pts.length == 1) return _dist(cx, cy, pts[0].x, pts[0].y) <= r + half;
  for (var i = 1; i < pts.length; i++) {
    if (_segDist(cx, cy, pts[i - 1].x, pts[i - 1].y, pts[i].x, pts[i].y) <= r + half) return true;
  }
  return false;
}

double _dist(double ax, double ay, double bx, double by) => math.sqrt((ax - bx) * (ax - bx) + (ay - by) * (ay - by));

double _segDist(double px, double py, double ax, double ay, double bx, double by) {
  final dx = bx - ax, dy = by - ay;
  final len2 = dx * dx + dy * dy;
  if (len2 == 0) return _dist(px, py, ax, ay);
  final t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
  return _dist(px, py, ax + t * dx, ay + t * dy);
}
