package dev.opennote.booxink

import kotlin.math.roundToInt

/**
 * Where the native ink View goes so that it covers exactly the web
 * editor's canvas element.
 *
 * The web side reports the element's rect in CSS px relative to the WebView
 * viewport (getBoundingClientRect) and its devicePixelRatio. The View is a
 * child of the Activity content FrameLayout, so the WebView's own offset
 * inside that layout (status bar / cutout / IME insets when the WebView is not
 * edge-to-edge) is added. Rounding is done once, on the final edges, so the
 * four corners of the View and of the CSS rect agree to within half a pixel.
 */
data class OverlayPlacement(val left: Int, val top: Int, val width: Int, val height: Int) {
    companion object {
        fun compute(
            cssX: Double, cssY: Double, cssW: Double, cssH: Double, dpr: Double,
            webViewInWindow: IntArray, contentInWindow: IntArray,
        ): OverlayPlacement {
            val ox = (webViewInWindow[0] - contentInWindow[0]).toDouble()
            val oy = (webViewInWindow[1] - contentInWindow[1]).toDouble()
            val l = (ox + cssX * dpr).roundToInt()
            val t = (oy + cssY * dpr).roundToInt()
            val r = (ox + (cssX + cssW) * dpr).roundToInt()
            val b = (oy + (cssY + cssH) * dpr).roundToInt()
            return OverlayPlacement(l, t, (r - l).coerceAtLeast(1), (b - t).coerceAtLeast(1))
        }
    }

    /** Document point -> window px, given the host view transform (same math as the web canvas). */
    fun docToWindow(t: ViewTransform, x: Float, y: Float, contentInWindow: IntArray): Pair<Float, Float> =
        Pair(contentInWindow[0] + left + t.viewX(x), contentInWindow[1] + top + t.viewY(y))
}
