import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:m00_flutter_app/ink/geometry.dart';
import 'package:m00_flutter_app/ink/model.dart';

String fixture(String p) => File('../../tests/fixtures/$p').readAsStringSync();

void main() {
  final golden = jsonDecode(fixture('geometry-golden.json')) as Map<String, dynamic>;
  final strokes = {for (final s in parseDocument(fixture('sample-strokes.json'))) s.id: s};

  test('width mapping matches golden', () {
    for (final c in golden['widthCases'] as List) {
      final w = widthFor((c['base'] as num).toDouble(), (c['pressure'] as num).toDouble(), c['pressureEnabled'] as bool);
      expect(w, closeTo((c['width'] as num).toDouble(), 1e-12));
    }
  });

  test('pieces match golden', () {
    for (final g in golden['strokes'] as List) {
      final s = strokes[g['id']]!;
      final pieces = strokePieces(s.points, s.brush.width, s.brush.pressure);
      final expected = g['pieces'] as List;
      expect(pieces.length, expected.length, reason: s.id);
      for (var i = 0; i < pieces.length; i++) {
        final e = expected[i] as Map<String, dynamic>;
        final p = pieces[i];
        double v(String k) => (e[k] as num).toDouble();
        expect(p.width, closeTo(v('width'), 1e-9));
        switch (p) {
          case DotPiece():
            expect(e['kind'], 'dot');
            expect(p.x, closeTo(v('x'), 1e-9));
          case LinePiece():
            expect(e['kind'], 'line');
            expect([p.x0, p.y0, p.x1, p.y1], [for (final k in ['x0', 'y0', 'x1', 'y1']) closeTo(v(k), 1e-9)]);
          case QuadPiece():
            expect(e['kind'], 'quad');
            expect([p.x0, p.y0, p.cx, p.cy, p.x1, p.y1], [for (final k in ['x0', 'y0', 'cx', 'cy', 'x1', 'y1']) closeTo(v(k), 1e-9)]);
        }
      }
    }
  });

  test('stable pieces are unchanged by appended points', () {
    final pts = [for (var i = 0; i < 20; i++) InkPoint(i * 3.0, (i % 4) * 2.0, 0.1 + i * 0.04)];
    final full = strokePieces(pts, 3, true);
    for (var n = 3; n <= pts.length; n++) {
      final part = strokePieces(pts.sublist(0, n), 3, true);
      for (var k = 0; k < stablePieceCount(n); k++) {
        expect(part[k].width, full[k].width);
      }
    }
  });

  test('eraser hit test', () {
    final s = strokes['sample-line']!;
    expect(hitTest(s, 300, 130, 4), isTrue);
    expect(hitTest(s, 300, 200, 4), isFalse);
  });
}
