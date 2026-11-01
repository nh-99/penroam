Source files: 6

### flutter — 3 session(s)

- system: `{"os": "linux", "osVersion": "Linux 7.2.6-76070206-generic #202609141300~1792433904~24.04~141c6f5 SMP PREEMPT_DYNAMIC Mon O", "dartVersion": "3.13.7 (stable) (Thu Oct 22 14:28:57 2026 -0700) on \"linux_x64\"", "numberOfProcessors": 24, "sessionType": "wayland", "desktop": "GNOME", "kernel": "7.2.6-76070206-generic", "releaseMode": true, "profileMode": false}`
- viewport: `{"width": 1280.0, "height": 720.0, "dpr": 2.0}`
- startup (process start → first frame/IPC), per session ms: ['220', '220', '240']

| strokes | trials | generate ms | full redraw ms (GPU-flushed) | replay frame interval median/max ms | memory MB |
|---:|---:|---:|---:|---:|---:|
| 100 | 9 | 0.5 | 11 | 17/18 | 215 |
| 1000 | 9 | 4.2 | 60 | 17/19 | 271 |
| 5000 | 9 | 24 | 280 | 17/18 | 396 |
| 10000 | 9 | 42 | 476 | 17/18 | 483 |

frame timing per session (median/p95/max ms): [('3.3', '16', '305'), ('3.2', '7.9', '239'), ('3.2', '9.8', '228')]

### tauri — 3 session(s)

- system: `{"os": "linux", "arch": "x86_64", "family": "unix", "kernel": "7.2.6-76070206-generic", "webviewVersion": "2.52.6", "tauriVersion": "2.12.5", "appVersion": "0.1.0", "sessionType": "wayland", "desktop": "GNOME"}`
- viewport: `{"width": 1280, "height": 733, "dpr": 2}`
- startup (process start → first frame/IPC), per session ms: ['2900', '3200', '440']

| strokes | trials | generate ms | full redraw ms (GPU-flushed) | replay frame interval median/max ms | memory MB |
|---:|---:|---:|---:|---:|---:|
| 100 | 9 | 2.0 | 4.0 | 16/20 | 499 |
| 1000 | 9 | 4.0 | 26 | 17/20 | 519 |
| 5000 | 9 | 20 | 130 | 16/21 | 630 |
| 10000 | 9 | 34 | 202 | 16/21 | 701 |

frame timing per session (median/p95/max ms): [('16', '20', '133'), ('16', '20', '131'), ('16', '20', '132')]

### Tauri vs Flutter (matched metrics, medians across all trials)

| strokes | full redraw ms Tauri / Flutter | replay interval median ms Tauri / Flutter | replay interval max ms Tauri / Flutter | memory MB Tauri / Flutter |
|---:|---:|---:|---:|---:|
| 100 | 4.0 / 11 | 16 / 17 | 20 / 18 | 499 / 215 |
| 1000 | 26 / 60 | 17 / 17 | 20 / 19 | 519 / 271 |
| 5000 | 130 / 280 | 16 / 17 | 21 / 18 | 630 / 396 |
| 10000 | 202 / 476 | 16 / 17 | 21 / 18 | 701 / 483 |

