package ink.penroam.app

import android.os.Bundle
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

/**
 * The app's only activity.
 *
 * The app draws edge to edge, but Android WebView does not reliably report the system bars
 * through CSS `env(safe-area-inset-*)`, and it never reports the bottom gesture area. Without
 * that, the bottom toolbar sits on top of the gesture navigation strip, and swiping along it
 * switches apps instead of scrolling the toolbar. So this activity measures the insets itself
 * and hands them to the page as CSS variables (`--pr-native-inset-top|right|bottom|left`, in
 * CSS pixels). `main.css` takes the larger of those and `env()`.
 */
class MainActivity : TauriActivity() {
  @Volatile private var insetsJson = """{"top":0,"right":0,"bottom":0,"left":0}"""

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }

  override fun onWebViewCreate(webView: WebView) {
    // Wry calls this before it loads the first page, so the interface is present from the
    // start. The page reads it once at startup, which also covers reloads: a value pushed
    // with evaluateJavascript before the document exists is lost.
    webView.addJavascriptInterface(InsetsBridge(), "PenroamInsets")

    ViewCompat.setOnApplyWindowInsetsListener(webView) { view, insets ->
      val bars = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
      )
      // With gesture navigation the bottom gesture zone is taller than the handle drawn in
      // the navigation bar, and apps can't opt out of it, so keep controls above it.
      val gestures = insets.getInsets(WindowInsetsCompat.Type.mandatorySystemGestures())
      val density = view.resources.displayMetrics.density
      fun css(px: Int) = px / density
      val top = css(bars.top)
      val right = css(bars.right)
      val bottom = css(maxOf(bars.bottom, gestures.bottom))
      val left = css(bars.left)
      insetsJson = """{"top":$top,"right":$right,"bottom":$bottom,"left":$left}"""
      view.post {
        (view as WebView).evaluateJavascript(
          "window.__penroamApplyInsets && window.__penroamApplyInsets($insetsJson)", null
        )
      }
      // Leave the insets unconsumed so the WebView still sees them.
      insets
    }
    ViewCompat.requestApplyInsets(webView)
  }

  private inner class InsetsBridge {
    @JavascriptInterface
    fun get(): String = insetsJson
  }
}
