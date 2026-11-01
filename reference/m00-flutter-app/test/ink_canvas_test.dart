import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:m00_flutter_app/ink/ink_canvas.dart';

void main() {
  testWidgets('stylus drag commits one stroke; touch pans instead', (tester) async {
    final c = InkController();
    await tester.pumpWidget(Directionality(textDirection: TextDirection.ltr, child: InkCanvas(controller: c)));

    final g = await tester.startGesture(const Offset(100, 100), kind: PointerDeviceKind.stylus);
    for (var i = 1; i <= 10; i++) {
      await g.moveTo(Offset(100 + i * 5.0, 100 + i * 2.0));
      await tester.pump();
    }
    await g.up();
    await tester.pump();
    expect(c.strokes.length, 1);
    expect(c.strokes.single.source, 'stylus');
    expect(c.strokes.single.points.length, greaterThanOrEqualTo(11));

    final t = await tester.startGesture(const Offset(200, 200), kind: PointerDeviceKind.touch);
    await t.moveTo(const Offset(240, 220));
    await t.up();
    await tester.pump();
    expect(c.strokes.length, 1, reason: 'touch navigates, does not draw');
    expect(c.transform.ox, 40);

    c.tool = Tool.eraser;
    final e = await tester.startGesture(const Offset(140 + 40, 116 + 20), kind: PointerDeviceKind.stylus);
    await e.up();
    await tester.pump();
    expect(c.strokes, isEmpty, reason: 'stroke eraser removes the hit stroke');
  });
}
