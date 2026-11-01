Source files: 6

### ios-native-uikit — 3 session(s)

- system: `{"lowPowerMode": false, "machine": "iPad13,4", "maxFPS": 120, "model": "iPad", "os": "ios", "osVersion": "26.6.2", "screen": {"h": 834, "w": 1194}, "screenScale": 2, "thermalState": 0}`
- viewport: `{"dpr": 2, "height": 657.5, "width": 1186}`
- startup (process start → first frame/IPC), per session ms: ['–', '–', '–']

| strokes | trials | generate ms | full redraw ms (flushed) | replay frame median/max ms | phys footprint MB |
|---:|---:|---:|---:|---:|---:|
| 100 | 9 | 3.1 | 18 | 0.3/1.1 | 23 |
| 1000 | 9 | 33 | 53 | 0.1/0.8 | 26 |
| 5000 | 9 | 94 | 262 | 0.1/0.7 | 33 |
| 10000 | 9 | 145 | 537 | 0.1/0.7 | 39 |

frame timing per session (median/p95/max ms): [('8.3', '8.3', '172'), ('8.3', '8.3', '171'), ('8.3', '8.3', '171')]

### tauri-frontend@ios-wkwebview — 3 session(s)

- system: `{"model": "iPad", "lowPowerMode": false, "machine": "iPad13,4", "screen": {"w": 1194, "h": 834}, "host": "M00iOS (Tauri frontend proxy)", "osVersion": "26.6.2", "maxFPS": 120, "thermalState": 0, "screenScale": 2, "os": "ios", "webview": "WKWebView"}`
- viewport: `{"width": 1186, "height": 597, "dpr": 2}`
- startup (process start → first frame/IPC), per session ms: ['11672', '42424', '64510']

| strokes | trials | generate ms | full redraw ms (flushed) | replay frame median/max ms | app footprint MB (WebContent excluded) |
|---:|---:|---:|---:|---:|---:|
| 100 | 9 | 2.0 | 5.0 | 0.0/1.0 | 18 |
| 1000 | 9 | 7.0 | 26 | 0.0/1.0 | 16 |
| 5000 | 9 | 29 | 37 | 0.0/1.0 | 16 |
| 10000 | 9 | 51 | 72 | 0.0/1.0 | 15 |

frame timing per session (median/p95/max ms): [('17', '17', '194'), ('17', '18', '197'), ('17', '17', '194')]

