// Minimal XTest input driver for Linux E2E sessions (XWayland, GDK_BACKEND=x11).
// Reads commands from stdin, one per line:
//   move X Y            absolute pointer move (screen px)
//   down B / up B       mouse button (1 left, 2 middle, 3 right, 4/5 wheel)
//   drag X0 Y0 X1 Y1 N  press left, N interpolated moves, release (5 ms apart)
//   key NAME            press+release keysym (e.g. Return, Escape, z); prefix with C- / S- for Ctrl/Shift
//   type TEXT           types ASCII text
//   sleep MS
//   close WINDOW_ID     send WM_DELETE_WINDOW (polite window close)
// Build: cc -O2 -o xdrive xdrive.c -lX11 -lXtst
#include <X11/Xlib.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>
#include <X11/XKBlib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static Display *d;
static void flush(void) { XFlush(d); XSync(d, False); }
static void key_sym(KeySym ks, int ctrl, int shift) {
  KeyCode kc = XKeysymToKeycode(d, ks);
  KeyCode c = XKeysymToKeycode(d, XK_Control_L), s = XKeysymToKeycode(d, XK_Shift_L);
  if (ctrl) XTestFakeKeyEvent(d, c, True, 0);
  if (shift) XTestFakeKeyEvent(d, s, True, 0);
  XTestFakeKeyEvent(d, kc, True, 0);
  XTestFakeKeyEvent(d, kc, False, 0);
  if (shift) XTestFakeKeyEvent(d, s, False, 0);
  if (ctrl) XTestFakeKeyEvent(d, c, False, 0);
  flush();
  usleep(15000);
}
int main(void) {
  d = XOpenDisplay(NULL);
  if (!d) { fprintf(stderr, "no X display\n"); return 1; }
  char line[4096];
  while (fgets(line, sizeof line, stdin)) {
    line[strcspn(line, "\n")] = 0;
    int a, b, c, e, n;
    if (sscanf(line, "move %d %d", &a, &b) == 2) { XTestFakeMotionEvent(d, -1, a, b, 0); flush(); }
    else if (sscanf(line, "down %d", &a) == 1) { XTestFakeButtonEvent(d, a, True, 0); flush(); }
    else if (sscanf(line, "up %d", &a) == 1) { XTestFakeButtonEvent(d, a, False, 0); flush(); }
    else if (sscanf(line, "drag %d %d %d %d %d", &a, &b, &c, &e, &n) == 5) {
      XTestFakeMotionEvent(d, -1, a, b, 0); flush(); usleep(20000);
      XTestFakeButtonEvent(d, 1, True, 0); flush(); usleep(10000);
      for (int i = 1; i <= n; i++) { XTestFakeMotionEvent(d, -1, a + (c - a) * i / n, b + (e - b) * i / n, 0); flush(); usleep(5000); }
      XTestFakeButtonEvent(d, 1, False, 0); flush(); usleep(30000);
    }
    else if (strncmp(line, "key ", 4) == 0) {
      const char *k = line + 4; int ctrl = 0, shift = 0;
      for (;;) { if (!strncmp(k, "C-", 2)) { ctrl = 1; k += 2; } else if (!strncmp(k, "S-", 2)) { shift = 1; k += 2; } else break; }
      KeySym ks = XStringToKeysym(k);
      if (ks == NoSymbol) { fprintf(stderr, "bad key %s\n", k); continue; }
      key_sym(ks, ctrl, shift);
    }
    else if (strncmp(line, "type ", 5) == 0) {
      // printable ASCII keysyms equal their character codes; "\\" types Return
      for (const char *p = line + 5; *p; p++) {
        if (*p == '\\') { key_sym(XK_Return, 0, 0); continue; }
        KeySym ks = (KeySym)(unsigned char)*p;
        KeyCode kc = XKeysymToKeycode(d, ks);
        if (!kc) continue;
        int shift = XkbKeycodeToKeysym(d, kc, 0, 0) != ks;
        key_sym(ks, 0, shift);
      }
    }
    else if (sscanf(line, "sleep %d", &a) == 1) usleep(a * 1000);
    else if (strncmp(line, "close ", 6) == 0) {
      // polite close (WM_DELETE_WINDOW), like clicking the window's close button
      Window w = (Window)strtoul(line + 6, NULL, 0);
      XEvent ev; memset(&ev, 0, sizeof ev);
      ev.xclient.type = ClientMessage; ev.xclient.window = w;
      ev.xclient.message_type = XInternAtom(d, "WM_PROTOCOLS", False);
      ev.xclient.format = 32;
      ev.xclient.data.l[0] = XInternAtom(d, "WM_DELETE_WINDOW", False);
      ev.xclient.data.l[1] = CurrentTime;
      XSendEvent(d, w, False, NoEventMask, &ev); flush();
    }
  }
  XCloseDisplay(d);
  return 0;
}
