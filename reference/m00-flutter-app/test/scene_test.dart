import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:m00_flutter_app/ink/metrics.dart';
import 'package:m00_flutter_app/ink/model.dart';
import 'package:m00_flutter_app/ink/scene.dart';

void main() {
  for (final n in [100, 1000]) {
    test('generateScene($n) reproduces fixtures/benchmark-scenes/scene-$n.json', () {
      final expected = parseDocument(File('../../tests/fixtures/benchmark-scenes/scene-$n.json').readAsStringSync());
      final actual = generateScene(n);
      expect(actual.length, expected.length);
      for (var i = 0; i < n; i++) {
        expect(actual[i].id, expected[i].id);
        expect(actual[i].brush.width, expected[i].brush.width);
        expect(actual[i].points, expected[i].points, reason: actual[i].id);
      }
    });
  }

  test('stroke JSON round-trips', () {
    final s = parseDocument(File('../../tests/fixtures/sample-strokes.json').readAsStringSync());
    final back = parseDocument(jsonEncode(toDocument(s)));
    for (var i = 0; i < s.length; i++) {
      expect(back[i].points, s[i].points);
      expect(back[i].brush.pressure, s[i].brush.pressure);
    }
  });

  test('metrics summary and gap analysis', () {
    final s = summarize([for (var i = 1; i <= 100; i++) i.toDouble()]);
    expect(s['median'], 51);
    expect(s['p95'], 95);
    final g = analyzeSamples([0, 4, 8, 200, 204], [0, 1, 2, 3, 300], [0, 0, 0, 0, 0]);
    expect(g['timeGaps'], 1);
    expect(g['spatialJumps'], 1);
  });
}
