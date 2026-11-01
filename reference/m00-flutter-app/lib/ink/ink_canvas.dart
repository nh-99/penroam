// Flutter framework drawing (Mode A): Listener + CustomPainter.
//
// Layers (PLAN.md §5):
//   raw input samples  -> InkController._active.points (+ engine timestamps)
//   active preview     -> _ActivePainter (own RepaintBoundary, repaints per
//                         frame while drawing; stable pieces are recorded into
//                         ui.Picture chunks so per-frame work stays bounded)
//   committed strokes  -> InkController.strokes (vector source of truth)
//   rendered cache     -> _CommittedPainter: ui.Picture chunks in document
//                         space, rasterized into one view-space ui.Image
//                         (cacheCommitted, default). Impeller has no raster
//                         cache, so without the image every frame re-renders
//                         every committed path. Same strategy as the Tauri
//                         committed canvas: full rebuild on load/erase/
//                         transform/resize, one stroke drawn per commit, one
//                         image blit per frame.
//
// No setState / widget rebuild happens per pointer event: painters listen to
// ChangeNotifiers directly via CustomPainter(repaint: ...).
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart' show HardwareKeyboard;
import 'package:flutter/widgets.dart';

import 'geometry.dart';
import 'metrics.dart';
import 'model.dart';

enum Tool { pen, eraser }

class ViewTransform {
  const ViewTransform(this.scale, this.ox, this.oy);
  static const identity = ViewTransform(1, 0, 0);
  final double scale, ox, oy; // offset in logical px
  Offset toDoc(Offset local) => Offset((local.dx - ox) / scale, (local.dy - oy) / scale);
  @override
  bool operator ==(Object o) => o is ViewTransform && o.scale == scale && o.ox == ox && o.oy == oy;
  @override
  int get hashCode => Object.hash(scale, ox, oy);
}

/// Repaint signal for CustomPainter(repaint:) without widget rebuilds.
class Tick extends ChangeNotifier {
  void bump() => notifyListeners();
}

class _Active {
  _Active(this.stroke, this.pointer, this.device, this.eraser);
  final InkStroke stroke;
  final int pointer;
  final String device;
  final bool eraser;
  final List<double> ts = []; // PointerEvent.timeStamp in ms (engine clock)
  Duration? t0;
  final List<ui.Picture> chunks = [];
  int chunked = 0; // pieces recorded into chunks
  final List<String> erased = [];
}

const double eraserRadius = 8;
const double _palmRadiusLogical = 20; // radiusMajor above this = palm
const int _chunkPieces = 64;
const int _committedChunk = 64;

class InkController {
  final Metrics metrics = Metrics();
  final List<InkStroke> strokes = [];
  ViewTransform transform = ViewTransform.identity;

  Tool tool = Tool.pen;
  double width = 3;
  bool pressure = true;
  RenderStyle renderStyle = RenderStyle.smooth;
  bool fingerDraws = false;
  bool inputEnabled = true;

  /// Notifies the committed layer (commit, erase, load, transform).
  final Tick committedTick = Tick();
  /// Notifies the active layer (every new sample while drawing).
  final Tick activeTick = Tick();
  VoidCallback? onStrokesChanged;
  ValueChanged<ViewTransform>? onTransform;

  _Active? _active;
  final List<ui.Picture> _committedPictures = [];
  final List<InkStroke> _pendingCommitted = []; // not yet in a picture chunk
  bool _picturesDirty = true;
  double _pendingSince = -1;
  final Map<int, Offset> _touches = {};
  double? _pinchDist, _pinchScale;
  Offset? _pinchAnchor;
  bool _penDown = false;

  /// Rasterize committed strokes into a view-space image (see header).
  bool cacheCommitted = true;
  double dpr = 1;
  ui.Image? _cache;
  ViewTransform? _cacheT;
  Size? _cacheSize;
  double _cacheDpr = 0;
  final List<InkStroke> _uncached = []; // committed since the image was built
  Size? _lastSize;

  // ------------------------------------------------------------ public --
  void setStrokes(List<InkStroke> s) {
    strokes
      ..clear()
      ..addAll(s);
    _invalidateCommitted();
  }

  void clear() {
    strokes.clear();
    _active = null;
    metrics.event('clear_canvas');
    _invalidateCommitted();
    _bump(activeTick);
    onStrokesChanged?.call();
  }

  void addCommittedStroke(InkStroke s) {
    strokes.add(s);
    _pendingCommitted.add(s);
    _uncached.add(s);
    _bump(committedTick);
    onStrokesChanged?.call();
  }

  void removeStrokes(Iterable<String> ids) {
    final set = ids.toSet();
    strokes.removeWhere((s) => set.contains(s.id));
    _invalidateCommitted();
    onStrokesChanged?.call();
  }

  void setTransform(ViewTransform t, {bool notify = false}) {
    transform = ViewTransform(t.scale.clamp(0.25, 8.0), t.ox, t.oy);
    _bump(committedTick); // pictures are in doc space: no re-record; the image is rebuilt
    _bump(activeTick);
    if (notify) onTransform?.call(transform);
  }

  void setRenderStyle(RenderStyle s) {
    if (s == renderStyle) return;
    renderStyle = s;
    _invalidateCommitted();
  }

  /// Rebuilds all committed pictures now; returns ms (recording only — raster
  /// happens on the raster thread and is reported through FrameTiming).
  double rebuildPicturesNow() {
    final t0 = nowMs();
    for (final p in _committedPictures) {
      p.dispose();
    }
    _committedPictures.clear();
    _pendingCommitted.clear();
    _disposeCache();
    for (var i = 0; i < strokes.length; i += _committedChunk) {
      final rec = ui.PictureRecorder();
      final c = Canvas(rec);
      for (final s in strokes.sublist(i, math.min(strokes.length, i + _committedChunk))) {
        drawStroke(c, s, renderStyle);
      }
      _committedPictures.add(rec.endRecording());
    }
    _picturesDirty = false;
    final ms = nowMs() - t0;
    metrics.timing('picture_record_ms', ms);
    metrics.count('full_rebuilds');
    return ms;
  }

  void _invalidateCommitted() {
    _picturesDirty = true;
    _disposeCache();
    _bump(committedTick);
  }

  void _disposeCache() {
    _cache?.dispose();
    _cache = null;
    _uncached.clear();
  }

  /// Full raster of all committed strokes into the view-space image.
  void _buildCache(Size size) {
    final t0 = nowMs();
    _cache?.dispose();
    final w = math.max(1, (size.width * dpr).ceil()), h = math.max(1, (size.height * dpr).ceil());
    final rec = ui.PictureRecorder();
    final c = Canvas(rec);
    c.scale(dpr);
    c.translate(transform.ox, transform.oy);
    c.scale(transform.scale);
    for (final p in _committedPictures) {
      c.drawPicture(p);
    }
    for (final s in _pendingCommitted) {
      drawStroke(c, s, renderStyle);
    }
    final pic = rec.endRecording();
    _cache = pic.toImageSync(w, h);
    pic.dispose();
    _cacheT = transform;
    _cacheSize = size;
    _cacheDpr = dpr;
    _uncached.clear();
    metrics.timing('cache_build_submit_ms', nowMs() - t0);
    metrics.count('cache_rebuilds');
  }

  /// Draws only the strokes committed since the last build into a new image.
  void _foldIntoCache() {
    final old = _cache!;
    final rec = ui.PictureRecorder();
    final c = Canvas(rec);
    c.drawImage(old, Offset.zero, Paint());
    c.scale(dpr);
    c.translate(transform.ox, transform.oy);
    c.scale(transform.scale);
    for (final s in _uncached) {
      drawStroke(c, s, renderStyle);
    }
    final pic = rec.endRecording();
    _cache = pic.toImageSync(old.width, old.height);
    pic.dispose();
    old.dispose();
    _uncached.clear();
  }

  /// Workload D: full rebuild (record + rasterize the committed image), then
  /// a 1x1 readback that depends on it, so the time includes GPU completion.
  /// Same idea as the Tauri engine's getImageData(0,0,1,1) flush.
  Future<double> rebuildFlushedMs() async {
    final t0 = nowMs();
    rebuildPicturesNow();
    final size = _lastSize;
    if (!cacheCommitted || size == null) return nowMs() - t0;
    _buildCache(size);
    final rec = ui.PictureRecorder();
    Canvas(rec).drawImageRect(_cache!, const Rect.fromLTWH(0, 0, 1, 1), const Rect.fromLTWH(0, 0, 1, 1), Paint());
    final pic = rec.endRecording();
    final px = pic.toImageSync(1, 1);
    pic.dispose();
    await px.toByteData();
    px.dispose();
    final ms = nowMs() - t0;
    metrics.timing('full_rebuild_flushed_ms', ms);
    _bump(committedTick);
    return ms;
  }

  void _bump(Tick n) => n.bump();

  // ------------------------------------------------------------- input --
  static String deviceName(PointerDeviceKind k) => switch (k) {
        PointerDeviceKind.stylus => 'stylus',
        PointerDeviceKind.invertedStylus => 'eraser',
        PointerDeviceKind.mouse => 'mouse',
        PointerDeviceKind.touch => 'touch',
        PointerDeviceKind.trackpad => 'trackpad',
        _ => 'unknown',
      };

  void onDown(PointerDownEvent e) {
    final t = nowMs();
    metrics.count('pointer_event_received');
    _recordDevice(e);
    if (!inputEnabled) return;
    final kind = e.kind;
    if (kind == PointerDeviceKind.touch) {
      if (_penDown || (_active != null && _active!.device != 'touch')) return metrics.count('palm_rejected_while_stylus');
      if (e.radiusMajor > _palmRadiusLogical) return metrics.count('palm_rejected_large_contact');
      if (!(fingerDraws && _touches.isEmpty && _active == null)) {
        _touches[e.pointer] = e.localPosition;
        if (_touches.length == 2) _beginPinch();
        return;
      }
    }
    if (kind == PointerDeviceKind.mouse && (e.buttons & kSecondaryMouseButton) != 0) return;
    if (kind == PointerDeviceKind.mouse && (e.buttons & kMiddleMouseButton) != 0) {
      _touches[e.pointer] = e.localPosition; // middle-drag pans
      return;
    }
    if (_active != null) return;
    final isStylus = kind == PointerDeviceKind.stylus || kind == PointerDeviceKind.invertedStylus;
    if (isStylus) _penDown = true;
    final eraser = tool == Tool.eraser || kind == PointerDeviceKind.invertedStylus || (e.buttons & kPrimaryStylusButton) != 0;
    final device = deviceName(kind);
    final s = InkStroke(
      id: newId(),
      source: isStylus ? 'stylus' : device,
      input: {'api': 'flutter-pointer', 'device': device, 'pressureMin': e.pressureMin, 'pressureMax': e.pressureMax},
      brush: InkBrush(width: width, pressure: pressure && kind != PointerDeviceKind.mouse),
      points: [],
    );
    _active = _Active(s, e.pointer, device, eraser)..t0 = e.timeStamp;
    _addSample(e);
    _afterInput(t);
  }

  void onMove(PointerMoveEvent e) {
    final t = nowMs();
    metrics.count('pointer_event_received');
    if (_touches.containsKey(e.pointer)) return _navigate(e);
    final a = _active;
    if (a == null || a.pointer != e.pointer || !inputEnabled) return;
    _addSample(e);
    _afterInput(t);
  }

  void onUp(PointerEvent e, {bool cancelled = false}) {
    if (e.kind == PointerDeviceKind.stylus || e.kind == PointerDeviceKind.invertedStylus) _penDown = false;
    if (_touches.remove(e.pointer) != null) {
      if (_touches.length < 2) _pinchDist = null;
      if (_touches.isEmpty) onTransform?.call(transform);
      return;
    }
    final a = _active;
    if (a == null || a.pointer != e.pointer) return;
    if (cancelled) {
      metrics.count('stroke_cancelled');
    } else {
      final p = _point(e, a);
      final last = a.stroke.points.isEmpty ? null : a.stroke.points.last;
      if (last == null || last.x != p.x || last.y != p.y) _addSample(e);
    }
    _finish();
  }

  void onSignal(PointerSignalEvent e) {
    if (e is PointerScrollEvent) {
      final ctrl = HardwareKeyboard.instance.isControlPressed || HardwareKeyboard.instance.isMetaPressed;
      if (ctrl) {
        final a = transform.toDoc(e.localPosition);
        final s = (transform.scale * math.exp(-e.scrollDelta.dy * 0.002)).clamp(0.25, 8.0);
        setTransform(ViewTransform(s, e.localPosition.dx - a.dx * s, e.localPosition.dy - a.dy * s), notify: true);
      } else {
        setTransform(ViewTransform(transform.scale, transform.ox - e.scrollDelta.dx, transform.oy - e.scrollDelta.dy), notify: true);
      }
    }
  }

  void _recordDevice(PointerDownEvent e) {
    final key = 'device_${deviceName(e.kind)}';
    if (!metrics.info.containsKey(key)) {
      // Logged once per device kind: what the platform actually reports.
      metrics.info[key] = {
        'pressureMin': e.pressureMin, 'pressureMax': e.pressureMax, 'pressure': e.pressure,
        'tilt': e.tilt, 'orientation': e.orientation, 'radiusMajor': e.radiusMajor,
        'distanceMax': e.distanceMax, 'buttons': e.buttons, 'device': e.device,
      };
    }
  }

  InkPoint _point(PointerEvent e, _Active a) {
    final d = transform.toDoc(e.localPosition);
    double p;
    if (e.kind == PointerDeviceKind.mouse) {
      p = 0.5;
    } else if (e.pressureMax > e.pressureMin) {
      p = ((e.pressure - e.pressureMin) / (e.pressureMax - e.pressureMin)).clamp(0.0, 1.0);
    } else {
      p = 0.5;
    }
    // Flutter: tilt = radians from perpendicular, orientation = radians.
    // Convert to W3C-style tiltX/tiltY degrees (same as Kotlin).
    final tx = math.atan(math.tan(e.tilt) * math.sin(e.orientation)) * 180 / math.pi;
    final ty = math.atan(math.tan(e.tilt) * -math.cos(e.orientation)) * 180 / math.pi;
    final dt = e.timeStamp - (a.t0 ?? e.timeStamp);
    return InkPoint(d.dx, d.dy, p, tiltX: tx.isFinite ? tx : 0, tiltY: ty.isFinite ? ty : 0, timeUs: dt.inMicroseconds);
  }

  void _addSample(PointerEvent e) {
    final a = _active!;
    final p = _point(e, a);
    a.stroke.points.add(p);
    if (a.eraser) return _eraseAt(p);
    a.ts.add(e.timeStamp.inMicroseconds / 1000.0);
    metrics.count('input_samples');
  }

  void _afterInput(double handlerStart) {
    metrics.since('input_sample_processed_ms', handlerStart);
    if (_pendingSince < 0) _pendingSince = handlerStart;
    metrics.count('preview_update_scheduled');
    _bump(activeTick);
  }

  void _navigate(PointerMoveEvent e) {
    final prev = _touches[e.pointer]!;
    _touches[e.pointer] = e.localPosition;
    if (_touches.length >= 2 && _pinchDist != null) {
      final v = _touches.values.toList();
      final dist = math.max(1.0, (v[1] - v[0]).distance);
      final f = (v[0] + v[1]) / 2;
      final s = (_pinchScale! * dist / _pinchDist!).clamp(0.25, 8.0);
      setTransform(ViewTransform(s, f.dx - _pinchAnchor!.dx * s, f.dy - _pinchAnchor!.dy * s));
    } else if (_touches.length == 1) {
      final d = e.localPosition - prev;
      setTransform(ViewTransform(transform.scale, transform.ox + d.dx, transform.oy + d.dy));
    }
  }

  void _beginPinch() {
    final v = _touches.values.toList();
    _pinchDist = math.max(1.0, (v[1] - v[0]).distance);
    _pinchScale = transform.scale;
    _pinchAnchor = transform.toDoc((v[0] + v[1]) / 2);
  }

  void _eraseAt(InkPoint p) {
    final r = eraserRadius / transform.scale;
    final before = strokes.length;
    strokes.removeWhere((s) {
      final hit = hitTest(s, p.x, p.y, r);
      if (hit) _active?.erased.add(s.id);
      return hit;
    });
    if (strokes.length != before) {
      metrics.count('strokes_erased', before - strokes.length);
      _invalidateCommitted();
    }
  }

  void _finish() {
    final a = _active!;
    _active = null;
    for (final c in a.chunks) {
      c.dispose();
    }
    if (a.eraser) {
      metrics.event('erase_finished', {'removed': a.erased.length});
      onStrokesChanged?.call();
      _bump(activeTick);
      return;
    }
    final t0 = nowMs();
    metrics.event('stroke_finalized', {'points': a.stroke.points.length});
    strokes.add(a.stroke);
    _pendingCommitted.add(a.stroke);
    _uncached.add(a.stroke);
    _commitStartMs = t0;
    _bump(committedTick);
    _bump(activeTick);
    final pts = a.stroke.points;
    metrics.stroke({
      'id': a.stroke.id,
      'device': a.device,
      'samples': pts.length,
      'durationMs': a.ts.length > 1 ? a.ts.last - a.ts.first : 0,
      ...analyzeSamples(a.ts, [for (final p in pts) p.x], [for (final p in pts) p.y]),
    });
    onStrokesChanged?.call();
  }

  double _commitStartMs = -1;

  // ------------------------------------------------------------ paint --
  void paintCommitted(Canvas canvas, Size size) {
    final t0 = nowMs();
    if (_picturesDirty) rebuildPicturesNow();
    if (_pendingCommitted.length >= _committedChunk) {
      final rec = ui.PictureRecorder();
      final c = Canvas(rec);
      for (final s in _pendingCommitted) {
        drawStroke(c, s, renderStyle);
      }
      _committedPictures.add(rec.endRecording());
      _pendingCommitted.clear();
    }
    _lastSize = size;
    if (cacheCommitted) {
      if (_cache == null || _cacheT != transform || _cacheSize != size || _cacheDpr != dpr) {
        _buildCache(size);
      } else if (_uncached.isNotEmpty) {
        _foldIntoCache();
      }
      canvas.save();
      canvas.scale(1 / dpr);
      canvas.drawImage(_cache!, Offset.zero, Paint());
      canvas.restore();
    } else {
      canvas.save();
      canvas.translate(transform.ox, transform.oy);
      canvas.scale(transform.scale);
      for (final p in _committedPictures) {
        canvas.drawPicture(p);
      }
      for (final s in _pendingCommitted) {
        drawStroke(canvas, s, renderStyle);
      }
      canvas.restore();
    }
    metrics.since('committed_paint_ms', t0);
    if (_commitStartMs >= 0) {
      metrics.timing('persistent_render_completed_ms', nowMs() - _commitStartMs);
      _commitStartMs = -1;
    }
  }

  void paintActive(Canvas canvas, Size size) {
    final t0 = nowMs();
    final a = _active;
    if (a != null && !a.eraser) {
      final n = a.stroke.points.length;
      final stable = stablePieceCount(n);
      // Record full chunks of stable pieces once; replay them every frame.
      while (stable - a.chunked >= _chunkPieces) {
        final rec = ui.PictureRecorder();
        drawPieces(Canvas(rec), a.stroke, a.chunked, a.chunked + _chunkPieces, renderStyle);
        a.chunks.add(rec.endRecording());
        a.chunked += _chunkPieces;
      }
      canvas.save();
      canvas.translate(transform.ox, transform.oy);
      canvas.scale(transform.scale);
      for (final c in a.chunks) {
        canvas.drawPicture(c);
      }
      drawPieces(canvas, a.stroke, a.chunked, pieceCount(n), renderStyle);
      canvas.restore();
    }
    metrics.since('preview_render_submitted_ms', t0);
    if (_pendingSince >= 0) {
      metrics.timing('handler_to_paint_ms', nowMs() - _pendingSince);
      _pendingSince = -1;
    }
  }

  // ------------------------------------------------- scripted replay --
  /// Replays points through the active-preview + commit path, one sample per
  /// frame (Workload D "responsiveness while adding strokes").
  /// Time between successive replay frames (comparable with Tauri's).
  final List<double> lastReplayIntervals = [];
  Future<List<double>> replay(List<InkPoint> pts, {double width = 3}) async {
    final s = InkStroke(id: newId(), source: 'fixture', input: {'api': 'replay', 'device': 'script'}, brush: InkBrush(width: width), points: []);
    _active = _Active(s, -1, 'script', false);
    final frames = <double>[];
    lastReplayIntervals.clear();
    var lastStep = -1.0;
    for (final p in pts) {
      final now = nowMs();
      if (lastStep >= 0) lastReplayIntervals.add(now - lastStep);
      lastStep = now;
      s.points.add(p);
      _active!.ts.add(nowMs());
      final t0 = nowMs();
      _bump(activeTick);
      await SchedulerBinding.instance.endOfFrame;
      frames.add(nowMs() - t0);
    }
    _finish();
    await SchedulerBinding.instance.endOfFrame;
    return frames;
  }
}

class InkCanvas extends StatelessWidget {
  const InkCanvas({super.key, required this.controller});
  final InkController controller;

  @override
  Widget build(BuildContext context) {
    final c = controller;
    c.dpr = MediaQuery.devicePixelRatioOf(context);
    return Listener(
      behavior: HitTestBehavior.opaque,
      onPointerDown: c.onDown,
      onPointerMove: c.onMove,
      onPointerUp: (e) => c.onUp(e),
      onPointerCancel: (e) => c.onUp(e, cancelled: true),
      onPointerSignal: c.onSignal,
      child: ColoredBox(
        color: const Color(0xFFFFFFFF),
        child: Stack(fit: StackFit.expand, children: [
          RepaintBoundary(child: CustomPaint(painter: _CommittedPainter(c), isComplex: true, willChange: false)),
          RepaintBoundary(child: CustomPaint(painter: _ActivePainter(c))),
        ]),
      ),
    );
  }
}

class _CommittedPainter extends CustomPainter {
  _CommittedPainter(this.c) : super(repaint: c.committedTick);
  final InkController c;
  @override
  void paint(Canvas canvas, Size size) => c.paintCommitted(canvas, size);
  @override
  bool shouldRepaint(_CommittedPainter old) => old.c != c;
}

class _ActivePainter extends CustomPainter {
  _ActivePainter(this.c) : super(repaint: c.activeTick);
  final InkController c;
  @override
  void paint(Canvas canvas, Size size) => c.paintActive(canvas, size);
  @override
  bool shouldRepaint(_ActivePainter old) => old.c != c;
}
