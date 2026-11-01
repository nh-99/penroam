// Files, process stats and environment for the Flutter prototype.
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

Future<Directory> _dir(String sub) async {
  // Android: app-specific external storage, so `adb pull` works on release builds
  // (same location as the native reference app). Elsewhere: app support dir.
  final base = (Platform.isAndroid ? await getExternalStorageDirectory() : null) ?? await getApplicationSupportDirectory();
  final d = Directory('${base.path}/$sub');
  await d.create(recursive: true);
  return d;
}

String _stamp() => DateTime.now().millisecondsSinceEpoch.toString();

Future<String> saveStrokes(String json) async {
  final f = File('${(await _dir('strokes')).path}/strokes-${_stamp()}.json');
  await f.writeAsString(json);
  return f.path;
}

Future<List<String>> listStrokeFiles() async {
  final d = await _dir('strokes');
  final names = d.listSync().whereType<File>().map((f) => f.uri.pathSegments.last).where((n) => n.endsWith('.json')).toList()
    ..sort();
  return names.reversed.toList();
}

Future<String> loadStrokeFile(String name) async {
  if (name.contains('/') || name.contains('..')) throw ArgumentError('invalid name $name');
  return File('${(await _dir('strokes')).path}/$name').readAsString();
}

/// M00_RESULTS_DIR lets benchmark runs write straight into benchmarks/results.
Future<String> writePerf(String label, String json) async {
  final env = Platform.environment['M00_RESULTS_DIR'];
  final dir = (env != null && env.isNotEmpty) ? Directory(env) : await _dir('perf');
  await dir.create(recursive: true);
  final f = File('${dir.path}/perf-$label-${_stamp()}.json');
  await f.writeAsString(json);
  return f.path;
}

bool get autobenchRequested => Platform.environment['M00_AUTOBENCH'] == '1';

Map<String, dynamic> systemInfo() => {
      'os': Platform.operatingSystem,
      'osVersion': Platform.operatingSystemVersion,
      'dartVersion': Platform.version,
      'numberOfProcessors': Platform.numberOfProcessors,
      'sessionType': Platform.environment['XDG_SESSION_TYPE'],
      'desktop': Platform.environment['XDG_CURRENT_DESKTOP'],
      'kernel': _read('/proc/sys/kernel/osrelease')?.trim(),
      'releaseMode': kReleaseMode,
      'profileMode': kProfileMode,
    };

String? _read(String p) {
  try {
    return File(p).readAsStringSync();
  } catch (_) {
    return null;
  }
}

/// Process RSS/CPU. Flutter desktop renders in-process, so the single
/// process is the comparable figure to Tauri's process-tree total.
Map<String, dynamic> procStats() {
  final out = <String, dynamic>{'available': true, 'pid': pid, 'rssBytes': ProcessInfo.currentRss, 'maxRssBytes': ProcessInfo.maxRss};
  final stat = _read('/proc/self/stat');
  if (stat != null) {
    final rest = stat.substring(stat.lastIndexOf(')') + 2).split(' ');
    out['cpuSeconds'] = (int.parse(rest[11]) + int.parse(rest[12])) / 100.0;
  }
  return out;
}

/// ms from process start to now (Linux/Android via /proc; null elsewhere).
double? processAgeMs() {
  final stat = _read('/proc/self/stat');
  final up = _read('/proc/uptime');
  if (stat == null || up == null) return null;
  final rest = stat.substring(stat.lastIndexOf(')') + 2).split(' ');
  final startTicks = int.parse(rest[19]);
  final uptimeS = double.parse(up.split(' ').first);
  return (uptimeS - startTicks / 100.0) * 1000.0;
}
