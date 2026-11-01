import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui' show FrameTiming, FramePhase;

import 'package:flutter/foundation.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import 'boox_view.dart';
import 'ink/geometry.dart';
import 'ink/ink_canvas.dart';
import 'ink/metrics.dart';
import 'ink/model.dart';
import 'ink/scene.dart';
import 'platform.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const M00App());
}

enum RenderMode { framework, booxHardware, nativeSoftware }

const _modeLabel = {
  RenderMode.framework: 'A · Framework (CustomPainter)',
  RenderMode.booxHardware: 'B · BOOX hardware preview',
  RenderMode.nativeSoftware: 'C · Native Android software',
};
const _widths = [1.5, 3.0, 6.0];
const _scenes = [100, 1000, 5000, 10000];

class M00App extends StatelessWidget {
  const M00App({super.key});
  @override
  Widget build(BuildContext context) => WidgetsApp(
        title: 'M00 Flutter Ink',
        color: const Color(0xFF000000),
        debugShowCheckedModeBanner: false,
        builder: (context, _) => const DefaultTextStyle(
          style: TextStyle(color: Color(0xFF000000), fontSize: 14),
          child: InkHome(),
        ),
      );
}

class InkHome extends StatefulWidget {
  const InkHome({super.key});
  @override
  State<InkHome> createState() => _InkHomeState();
}

class _InkHomeState extends State<InkHome> {
  final InkController ink = InkController();
  Metrics get m => ink.metrics;

  Tool tool = Tool.pen;
  double width = 3;
  bool pressure = true;
  RenderStyle style = RenderStyle.smooth;
  RenderMode mode = RenderMode.framework;
  Composition composition = Composition.hybrid;
  bool debug = true;
  bool fingerDraws = false;
  String status = 'ready';
  Map<String, dynamic>? nativeStatus;
  List<String>? files;
  BooxViewController? native;
  final ValueNotifier<String> overlay = ValueNotifier('');
  Timer? _overlayTimer;
  bool _firstFrame = true;
  Map<String, dynamic>? _nativeOptions;

  @override
  void initState() {
    super.initState();
    m.info['system'] = systemInfo();
    SchedulerBinding.instance.addTimingsCallback(_onTimings);
    SchedulerBinding.instance.addPostFrameCallback((_) {
      if (_firstFrame) {
        _firstFrame = false;
        final age = processAgeMs();
        if (age != null) {
          m.timing('startup_to_first_frame_ms', age);
          m.info['startupMs'] = age;
        }
      }
    });
    _startOverlay();
    if (autobenchRequested) {
      WidgetsBinding.instance.addPostFrameCallback((_) async {
        await Future<void>.delayed(const Duration(milliseconds: 500));
        await runSceneBenchmark();
        exit(0);
      });
    }
  }

  @override
  void dispose() {
    SchedulerBinding.instance.removeTimingsCallback(_onTimings);
    _overlayTimer?.cancel();
    super.dispose();
  }

  /// Engine frame timings (build, raster, total span). These are the
  /// framework's own timing API; they do not include display scanout.
  void _onTimings(List<FrameTiming> timings) {
    for (final t in timings) {
      final build = t.buildDuration.inMicroseconds / 1000;
      final raster = t.rasterDuration.inMicroseconds / 1000;
      final total = t.totalSpan.inMicroseconds / 1000;
      m.timing('frame_build_ms', build);
      m.timing('frame_raster_ms', raster);
      m.timing('frame_total_span_ms', total);
      m.timing('frame_vsync_overhead_ms', t.vsyncOverhead.inMicroseconds / 1000);
      if (total > 16.7 * 1.5) m.count('frames_over_budget');
      m.count('framework_frame_presented');
      m.info['lastRasterFinishUs'] = t.timestampInMicroseconds(FramePhase.rasterFinish);
    }
  }

  void _startOverlay() {
    _overlayTimer?.cancel();
    if (!debug) return;
    // 4 Hz refresh via ValueNotifier: never rebuilt per pointer sample.
    _overlayTimer = Timer.periodic(const Duration(milliseconds: 250), (_) {
      String f(String k) {
        final s = m.get(k);
        return s?['median'] != null ? '${(s!['median'] as double).toStringAsFixed(1)}/${(s['p95'] as double).toStringAsFixed(1)}' : '-';
      }

      final last = m.strokes.isEmpty ? null : m.strokes.last;
      final ps = procStats();
      overlay.value = 'strokes ${ink.strokes.length} · events ${m.counters['pointer_event_received'] ?? 0} · samples ${m.counters['input_samples'] ?? 0} · over-budget frames ${m.counters['frames_over_budget'] ?? 0} · rss ${((ps['rssBytes'] as int) / 1048576).toStringAsFixed(0)}MB\n'
          'med/p95 ms  handler→paint ${f('handler_to_paint_ms')}  paint ${f('preview_render_submitted_ms')}  build ${f('frame_build_ms')}  raster ${f('frame_raster_ms')}  span ${f('frame_total_span_ms')}  commit ${f('persistent_render_completed_ms')}\n'
          '${last != null ? 'last stroke: ${last['device']} n=${last['samples']} dt med ${(last['medianDtMs'] as double).toStringAsFixed(1)} max ${(last['maxDtMs'] as double).toStringAsFixed(1)} gaps ${last['timeGaps']} jumps ${last['spatialJumps']}' : ''}\n'
          'devices: ${m.info.keys.where((k) => k.startsWith('device_')).map((k) => '$k ${jsonEncode(m.info[k])}').join(' ')}';
    });
  }

  void _applyBrush() {
    ink
      ..tool = tool
      ..width = width
      ..pressure = pressure
      ..fingerDraws = fingerDraws;
    ink.setRenderStyle(style);
    native?.setBrush(width, pressure, tool);
  }

  // --------------------------------------------------------- modes --
  Future<void> _switchMode(RenderMode next) async {
    if (mode != RenderMode.framework) {
      await native?.detach();
      native = null;
      nativeStatus = null;
    }
    setState(() {
      mode = next;
      ink.inputEnabled = next == RenderMode.framework;
      status = 'mode ${_modeLabel[next]}${next != RenderMode.framework ? ' · composition ${composition.name}' : ''}';
    });
  }

  Future<void> _onNativeCreated(BooxViewController c) async {
    native = c;
    c.onStroke = ink.addCommittedStroke;
    c.onErased = ink.removeStrokes;
    c.onViewport = (t) => ink.setTransform(t);
    c.onStatus = (s) {
      if (!mounted) return;
      setState(() => nativeStatus = s);
    };
    if (_nativeOptions != null) await c.setOptions(_nativeOptions!);
    final requested = mode == RenderMode.booxHardware ? NativeMode.booxHardware : NativeMode.nativeSoftware;
    final actual = await c.setMode(requested);
    final caps = await c.capabilities();
    m.info['nativeCapabilities'] = caps;
    if (!mounted) return;
    setState(() {
      status = actual == requested.wire
          ? 'mode ${_modeLabel[mode]} · composition ${composition.name}'
          : 'requested ${_modeLabel[mode]} — FELL BACK to $actual (${caps['errors']})';
    });
  }

  Map<String, dynamic> _creationParams(BuildContext context) => {
        'mode': mode == RenderMode.booxHardware ? 'BOOX_HARDWARE' : 'NATIVE_SOFTWARE',
        'width': width,
        'pressure': pressure,
        'tool': tool.name,
        'dpr': MediaQuery.of(context).devicePixelRatio,
        'scale': ink.transform.scale,
        'offsetX': ink.transform.ox,
        'offsetY': ink.transform.oy,
        'strokes': jsonEncode(toDocument(ink.strokes)),
      };

  // ------------------------------------------------------- actions --
  Future<void> _syncNative() async {
    if (native != null) await native!.loadStrokes(ink.strokes);
  }

  Future<void> _loadDoc(String text, String label) async {
    final t0 = nowMs();
    final strokes = parseDocument(text);
    final parseMs = nowMs() - t0;
    ink.setStrokes(strokes);
    final recMs = ink.rebuildPicturesNow();
    m.timing('load_parse_ms', parseMs);
    await SchedulerBinding.instance.endOfFrame;
    await _syncNative();
    setState(() => status = '$label: ${strokes.length} strokes, parse ${parseMs.toStringAsFixed(0)} ms, record ${recMs.toStringAsFixed(0)} ms');
  }

  Future<void> _loadScene(int n) async {
    final t0 = nowMs();
    final strokes = generateScene(n);
    final genMs = nowMs() - t0;
    ink.setStrokes(strokes);
    final recMs = ink.rebuildPicturesNow();
    await SchedulerBinding.instance.endOfFrame;
    await _syncNative();
    setState(() => status = 'scene $n: generate ${genMs.toStringAsFixed(0)} ms, record ${recMs.toStringAsFixed(0)} ms');
  }

  Future<void> _exportPerf() async {
    final out = {
      'app': 'flutter',
      'mode': mode.name,
      'composition': composition.name,
      'exportedAt': DateTime.now().toIso8601String(),
      'system': systemInfo(),
      'proc': procStats(),
      'metrics': m.toJson(),
      'native': native == null ? null : await native!.metrics(),
    };
    final where = await writePerf('flutter-${mode.name}', const JsonEncoder.withIndent('  ').convert(out));
    setState(() => status = 'perf → $where');
  }

  /// Workload D: generate, record pictures, raster (next frame), replay a stroke.
  Future<void> runSceneBenchmark() async {
    final sample = parseDocument(await rootBundle.loadString('assets/sample-strokes.json'));
    final replayPts = [for (final p in sample.firstWhere((s) => s.id == 'sample-curve').points) InkPoint(p.x + 200, p.y + 300, p.pressure, timeUs: p.timeUs)];
    final results = <Map<String, dynamic>>[];
    for (final n in _scenes) {
      for (var trial = 0; trial < 3; trial++) {
        setState(() => status = 'bench: scene $n trial ${trial + 1}/3');
        await Future<void>.delayed(const Duration(milliseconds: 50));
        final t0 = nowMs();
        final strokes = generateScene(n);
        final genMs = nowMs() - t0;
        final rasterBefore = m.get('frame_raster_ms')?['count'] as int? ?? 0;
        final t1 = nowMs();
        ink.setStrokes(strokes);
        final recordMs = ink.rebuildPicturesNow();
        await SchedulerBinding.instance.endOfFrame;
        final toFrameMs = nowMs() - t1;
        // Same measurement as Tauri's rebuildNow(): full redraw incl. GPU flush.
        final rebuildMs = await ink.rebuildFlushedMs();
        await SchedulerBinding.instance.endOfFrame;
        final frames = await ink.replay(replayPts);
        final sorted = [...frames]..sort();
        final iv = [...ink.lastReplayIntervals]..sort();
        final ps = procStats();
        results.add({
          'strokes': n, 'trial': trial, 'generateMs': genMs, 'recordMs': recordMs, 'loadToFrameEndMs': toFrameMs, 'rebuildMs': rebuildMs,
          'cacheCommitted': ink.cacheCommitted,
          'replayFrameMedianMs': sorted[sorted.length ~/ 2], 'replayFrameMaxMs': sorted.last, 'replayFrames': frames.length, 'replayIntervalMedianMs': iv.isEmpty ? null : iv[iv.length ~/ 2], 'replayIntervalMaxMs': iv.isEmpty ? null : iv.last,
          'rasterTimingsSoFar': (m.get('frame_raster_ms')?['count'] as int? ?? 0) - rasterBefore,
          'rssBytes': ps['rssBytes'],
        });
      }
    }
    ink.clear();
    await Future<void>.delayed(const Duration(milliseconds: 300)); // let FrameTimings flush
    final view = View.of(context);
    final out = {
      'app': 'flutter', 'workload': 'D-large-document', 'exportedAt': DateTime.now().toIso8601String(),
      'system': systemInfo(),
      'viewport': {'width': view.physicalSize.width / view.devicePixelRatio, 'height': view.physicalSize.height / view.devicePixelRatio, 'dpr': view.devicePixelRatio},
      'results': results, 'metrics': m.toJson(),
    };
    final where = await writePerf('flutter-workload-d', const JsonEncoder.withIndent('  ').convert(out));
    if (mounted) setState(() => status = 'bench done → $where');
  }

  /// Workload D-native ("Bench B"): large documents through the native BOOX view
  /// (Mode B or C). Times what differs between hosts: serializing the document,
  /// the platform-channel round trip (native parse + bitmap rebuild included),
  /// zoom / pan round trips (native rebuild), and an erase whose result event
  /// crosses back over the bridge. The framework canvas is not involved.
  /// Same procedure as the Tauri app's runNativeBench.
  Future<void> runNativeBench() async {
    final c = native;
    if (c == null) {
      setState(() => status = 'Bench B needs mode B or C');
      return;
    }
    ink.clear();
    final size = context.size ?? const Size(800, 600);
    final cx = size.width / 2, cy = size.height / 2;
    final results = <Map<String, dynamic>>[];
    for (final n in _scenes) {
      for (var trial = 0; trial < 3; trial++) {
        setState(() => status = 'bench B: scene $n trial ${trial + 1}/3');
        await Future<void>.delayed(const Duration(milliseconds: 50));
        final strokes = generateScene(n);
        final t0 = nowMs();
        final json = jsonEncode(toDocument(strokes));
        final serializeMs = nowMs() - t0;
        final t1 = nowMs();
        await c.loadStrokesJson(json);
        final loadMs = nowMs() - t1;
        final t2 = nowMs();
        await c.setTransform(ViewTransform(1.25, cx - cx * 1.25, cy - cy * 1.25));
        final zoomInMs = nowMs() - t2;
        final t3 = nowMs();
        await c.setTransform(ViewTransform.identity);
        final zoomOutMs = nowMs() - t3;
        final t4 = nowMs();
        await c.setTransform(const ViewTransform(1, -120, -80));
        final panMs = nowMs() - t4;
        await c.setTransform(ViewTransform.identity);
        final evt = Completer<double>();
        final prev = c.onErased;
        final t5 = nowMs();
        c.onErased = (ids) { if (!evt.isCompleted) evt.complete(nowMs() - t5); };
        final removed = await c.eraseAlong([for (var x = 100.0; x <= 300; x += 5) ...[x, 575.0]]);
        final eraseMs = nowMs() - t5;
        final eventMs = removed > 0 ? await evt.future.timeout(const Duration(seconds: 5), onTimeout: () => -1) : null;
        c.onErased = prev;
        results.add({
          'strokes': n, 'trial': trial, 'jsonBytes': json.length, 'serializeMs': serializeMs, 'loadRoundTripMs': loadMs,
          'zoomInMs': zoomInMs, 'zoomOutMs': zoomOutMs, 'panMs': panMs,
          'eraseRemoved': removed, 'eraseRoundTripMs': eraseMs, 'eraseEventMs': eventMs,
          'rssBytes': procStats()['rssBytes'],
        });
      }
    }
    final out = {
      'app': 'flutter', 'workload': 'D-native-boox', 'mode': mode.name, 'composition': composition.name,
      'exportedAt': DateTime.now().toIso8601String(), 'system': systemInfo(),
      'results': results, 'native': await c.metrics(),
    };
    final where = await writePerf('flutter-bench-b', const JsonEncoder.withIndent('  ').convert(out));
    if (mounted) setState(() => status = 'bench B done → $where');
  }

  void _zoom(double f) {
    final size = context.size ?? const Size(800, 600);
    final c = Offset(size.width / 2, size.height / 2);
    final a = ink.transform.toDoc(c);
    final s = f == 0 ? 1.0 : ink.transform.scale * f;
    final t = f == 0 ? ViewTransform.identity : ViewTransform(s, c.dx - a.dx * s, c.dy - a.dy * s);
    ink.setTransform(t);
    native?.setTransform(t);
  }

  // ------------------------------------------------------------ UI --
  Widget _btn(String label, VoidCallback onTap, {bool on = false}) => GestureDetector(
        onTap: onTap,
        child: Container(
          margin: const EdgeInsets.all(2),
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 5),
          decoration: BoxDecoration(color: on ? const Color(0xFF000000) : const Color(0xFFFFFFFF), border: Border.all(width: 2)),
          child: Text(label, style: TextStyle(color: on ? const Color(0xFFFFFFFF) : const Color(0xFF000000))),
        ),
      );

  void _set(VoidCallback f) {
    setState(f);
    _applyBrush();
  }

  @override
  Widget build(BuildContext context) {
    final android = defaultTargetPlatform == TargetPlatform.android;
    final modes = [RenderMode.framework, if (android) RenderMode.booxHardware, if (android) RenderMode.nativeSoftware];
    return ColoredBox(
      color: const Color(0xFFFFFFFF),
      child: SafeArea(
        child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
          Container(
            decoration: const BoxDecoration(border: Border(bottom: BorderSide(width: 2))),
            child: Wrap(children: [
              _btn('Pen', () => _set(() => tool = Tool.pen), on: tool == Tool.pen),
              _btn('Eraser', () => _set(() => tool = Tool.eraser), on: tool == Tool.eraser),
              for (final w in _widths) _btn('W$w', () => _set(() => width = w), on: width == w),
              _btn(pressure ? 'Pressure ON' : 'Pressure OFF', () => _set(() => pressure = !pressure), on: pressure),
              _btn('Clear', () {
                ink.clear();
                native?.clear();
              }),
              for (final md in modes) _btn(_modeLabel[md]!, () => _switchMode(md), on: mode == md),
              if (android)
                _btn('Composition: ${composition.name}', () async {
                  final next = Composition.values[(composition.index + 1) % Composition.values.length];
                  final current = mode;
                  if (current != RenderMode.framework) await _switchMode(RenderMode.framework);
                  setState(() => composition = next);
                  if (current != RenderMode.framework) await _switchMode(current);
                }),
              if (android && mode == RenderMode.booxHardware) ...[
                _btn('rawRender ${(_nativeOptions?['rawRenderEnabled'] ?? true) ? 'ON' : 'OFF'}', () {
                  final o = {...?_nativeOptions};
                  o['rawRenderEnabled'] = !(o['rawRenderEnabled'] ?? true);
                  setState(() => _nativeOptions = o);
                  native?.setOptions(o);
                }),
                _btn('Handoff ${_nativeOptions?['handoff'] ?? 'PEN_UP_REFRESH'}', () {
                  const hs = ['PEN_UP_REFRESH', 'TOGGLE_RAW', 'REGIONAL_GC'];
                  final o = {...?_nativeOptions};
                  o['handoff'] = hs[(hs.indexOf(o['handoff'] as String? ?? hs[0]) + 1) % hs.length];
                  setState(() => _nativeOptions = o);
                  native?.setOptions(o);
                }),
              ],
              _btn(style == RenderStyle.smooth ? 'Smoothed' : 'Raw polyline',
                  () => _set(() => style = style == RenderStyle.smooth ? RenderStyle.polyline : RenderStyle.smooth)),
              _btn('Save', () async {
                final p = await saveStrokes(jsonEncode(toDocument(ink.strokes, {'app': 'flutter'})));
                setState(() => status = 'saved $p');
              }),
              _btn('Load…', () async {
                final f = await listStrokeFiles();
                setState(() => files = f);
              }),
              _btn('Sample', () async => _loadDoc(await rootBundle.loadString('assets/sample-strokes.json'), 'sample')),
              for (final n in _scenes) _btn('Scene ${n >= 1000 ? '${n ~/ 1000}k' : n}', () => _loadScene(n)),
              _btn('Bench D', runSceneBenchmark),
              _btn('Bench B', runNativeBench),
              _btn('Export perf', _exportPerf),
              _btn('Reset perf', m.reset),
              _btn('Zoom −', () => _zoom(1 / 1.25)),
              _btn('1:1', () => _zoom(0)),
              _btn('Zoom +', () => _zoom(1.25)),
              _btn('Debug', () {
                setState(() => debug = !debug);
                _startOverlay();
              }, on: debug),
              _btn('Finger draws', () => _set(() => fingerDraws = !fingerDraws), on: fingerDraws),
            ]),
          ),
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
            decoration: const BoxDecoration(border: Border(bottom: BorderSide(width: 1))),
            child: Text(
              'MODE: ${_modeLabel[mode]} · $status'
              '${nativeStatus != null ? ' · native: ${nativeStatus!['mode']} raw=${nativeStatus!['sessionState']} coords=${nativeStatus!['rawCoordinateSpace']}${nativeStatus!['lastError'] != null ? ' · ERROR ${nativeStatus!['lastError']}' : ''}' : ''}',
              maxLines: 2,
              style: const TextStyle(fontWeight: FontWeight.bold),
            ),
          ),
          if (debug)
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
              decoration: const BoxDecoration(border: Border(bottom: BorderSide(width: 1))),
              child: ValueListenableBuilder<String>(
                valueListenable: overlay,
                builder: (_, v, __) => Text(v, style: const TextStyle(fontSize: 11, fontFamily: 'monospace')),
              ),
            ),
          if (files != null)
            Wrap(children: [
              if (files!.isEmpty) const Padding(padding: EdgeInsets.all(6), child: Text('no saved files')),
              for (final f in files!)
                _btn(f, () async {
                  setState(() => files = null);
                  await _loadDoc(await loadStrokeFile(f), f);
                }),
              _btn('Cancel', () => setState(() => files = null)),
            ]),
          Expanded(
            child: mode == RenderMode.framework
                ? InkCanvas(controller: ink)
                : BooxInkPlatformView(
                    key: ValueKey('${mode.name}-${composition.name}'),
                    composition: composition,
                    creationParams: _creationParams(context),
                    onCreated: _onNativeCreated,
                  ),
          ),
        ]),
      ),
    );
  }
}
