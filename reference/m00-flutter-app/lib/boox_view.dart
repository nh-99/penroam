// Embeds the shared native BooxInkView (android/boox-ink) through Flutter
// Platform Views. Used for Mode B (BOOX hardware preview) and Mode C (native
// Android software baseline).
//
// The composition technique is selectable because PLAN.md §6 requires
// evaluating them on the actual device instead of assuming the newest works:
//   hybrid    : PlatformViewLink + initExpensiveAndroidView (Hybrid Composition;
//               the real View sits in the Activity hierarchy at its true
//               screen position — expected to be needed by TouchHelper)
//   tlhc      : PlatformViewLink + initSurfaceAndroidView (Texture Layer HC;
//               falls back to HC where unsupported)
//   androidView: AndroidView widget (framework default: TLHC or virtual display)
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import 'ink/ink_canvas.dart';
import 'ink/model.dart';

const String booxViewType = 'dev.opennote/boox_ink';

enum Composition { hybrid, tlhc, androidView }

enum NativeMode { booxHardware, nativeSoftware }

extension NativeModeName on NativeMode {
  String get wire => this == NativeMode.booxHardware ? 'BOOX_HARDWARE' : 'NATIVE_SOFTWARE';
}

/// Dart side of one native view instance.
class BooxViewController {
  BooxViewController(int id) : _ch = MethodChannel('$booxViewType/$id');
  final MethodChannel _ch;

  ValueChanged<InkStroke>? onStroke;
  ValueChanged<List<String>>? onErased;
  ValueChanged<ViewTransform>? onViewport;
  ValueChanged<Map<String, dynamic>>? onStatus;

  void listen() {
    _ch.setMethodCallHandler((call) async {
      switch (call.method) {
        case 'onStroke':
          onStroke?.call(InkStroke.fromJson((jsonDecode(call.arguments as String) as Map).cast<String, dynamic>()));
        case 'onErased':
          onErased?.call((call.arguments as List).cast<String>());
        case 'onViewport':
          final m = (call.arguments as Map).cast<String, dynamic>();
          onViewport?.call(ViewTransform((m['scale'] as num).toDouble(), (m['offsetX'] as num).toDouble(), (m['offsetY'] as num).toDouble()));
        case 'onStatus':
          onStatus?.call((jsonDecode(call.arguments as String) as Map).cast<String, dynamic>());
      }
      return null;
    });
  }

  Future<String> setMode(NativeMode m) async => (await _ch.invokeMethod<String>('setMode', m.wire))!;
  Future<void> setBrush(double width, bool pressure, Tool tool) =>
      _ch.invokeMethod('setBrush', {'width': width, 'pressure': pressure, 'tool': tool.name});
  Future<void> setOptions(Map<String, dynamic> o) => _ch.invokeMethod('setOptions', jsonEncode(o));
  Future<void> setTransform(ViewTransform t) =>
      _ch.invokeMethod('setTransform', {'scale': t.scale, 'offsetX': t.ox, 'offsetY': t.oy});
  Future<void> clear() => _ch.invokeMethod('clear');
  Future<void> loadStrokes(List<InkStroke> s) => _ch.invokeMethod('loadStrokes', jsonEncode(toDocument(s)));
  Future<void> loadStrokesJson(String json) => _ch.invokeMethod('loadStrokes', json);
  /// Benchmarks: whole-stroke erase along a document-space path [x0, y0, x1, y1, ...].
  Future<int> eraseAlong(List<double> xy) async => (await _ch.invokeMethod<int>('eraseAlong', xy))!;
  Future<Map<String, dynamic>> capabilities() async =>
      (jsonDecode((await _ch.invokeMethod<String>('getCapabilities'))!) as Map).cast<String, dynamic>();
  Future<Map<String, dynamic>> metrics() async =>
      (jsonDecode((await _ch.invokeMethod<String>('getMetrics'))!) as Map).cast<String, dynamic>();
  Future<void> detach() => _ch.invokeMethod('detach');
}

class BooxInkPlatformView extends StatelessWidget {
  const BooxInkPlatformView({
    super.key,
    required this.composition,
    required this.creationParams,
    required this.onCreated,
  });

  final Composition composition;
  final Map<String, dynamic> creationParams;
  final ValueChanged<BooxViewController> onCreated;

  // All pointers go to the native view: it does its own palm rejection and
  // finger navigation, and in Mode B the Onyx raw reader owns the pen.
  static final Set<Factory<OneSequenceGestureRecognizer>> _recognizers = {
    Factory<OneSequenceGestureRecognizer>(() => EagerGestureRecognizer()),
  };

  void _created(int id) {
    final c = BooxViewController(id)..listen();
    onCreated(c);
  }

  @override
  Widget build(BuildContext context) {
    if (defaultTargetPlatform != TargetPlatform.android) {
      return const Center(child: Text('Native BOOX view is Android-only'));
    }
    if (composition == Composition.androidView) {
      return AndroidView(
        viewType: booxViewType,
        creationParams: creationParams,
        creationParamsCodec: const StandardMessageCodec(),
        gestureRecognizers: _recognizers,
        onPlatformViewCreated: _created,
      );
    }
    return PlatformViewLink(
      viewType: booxViewType,
      surfaceFactory: (context, controller) => AndroidViewSurface(
        controller: controller as AndroidViewController,
        gestureRecognizers: _recognizers,
        hitTestBehavior: PlatformViewHitTestBehavior.opaque,
      ),
      onCreatePlatformView: (params) {
        final AndroidViewController c = composition == Composition.hybrid
            ? PlatformViewsService.initExpensiveAndroidView(
                id: params.id,
                viewType: booxViewType,
                layoutDirection: TextDirection.ltr,
                creationParams: creationParams,
                creationParamsCodec: const StandardMessageCodec(),
                onFocus: () => params.onFocusChanged(true),
              )
            : PlatformViewsService.initSurfaceAndroidView(
                id: params.id,
                viewType: booxViewType,
                layoutDirection: TextDirection.ltr,
                creationParams: creationParams,
                creationParamsCodec: const StandardMessageCodec(),
                onFocus: () => params.onFocusChanged(true),
              );
        c
          ..addOnPlatformViewCreatedListener(params.onPlatformViewCreated)
          ..addOnPlatformViewCreatedListener(_created)
          ..create();
        return c;
      },
    );
  }
}
