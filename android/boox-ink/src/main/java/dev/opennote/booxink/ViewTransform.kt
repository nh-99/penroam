package dev.opennote.booxink

/**
 * Maps between view-local physical pixels (what MotionEvent and the Onyx
 * TouchHelper report, relative to the host View) and document units.
 *
 *   viewPx = (doc * scale + offset) * density
 *   doc    = (viewPx / density - offset) / scale
 *
 * `density` is the host framework's devicePixelRatio (Flutter
 * MediaQuery.devicePixelRatio, WebView window.devicePixelRatio), so that one
 * document unit equals one logical pixel at zoom 1.0 on every host. `offset`
 * is in logical px. Status bars, cutouts and the View's on-screen origin are
 * already removed because all coordinates are View-local; see
 * [BooxInkView.rawCoordinateSpace] for the probe that verifies the Onyx SDK
 * reports View-local and not screen coordinates on a given firmware.
 */
data class ViewTransform(
    val scale: Float = 1f,
    val offsetX: Float = 0f,
    val offsetY: Float = 0f,
    val density: Float = 1f,
) {
    fun docX(viewPx: Float): Float = (viewPx / density - offsetX) / scale
    fun docY(viewPx: Float): Float = (viewPx / density - offsetY) / scale
    fun viewX(doc: Float): Float = (doc * scale + offsetX) * density
    fun viewY(doc: Float): Float = (doc * scale + offsetY) * density

    /** Multiplier from document units to view pixels (for widths). */
    val docToPx: Float get() = scale * density

    val isIdentityZoom: Boolean get() = scale == 1f && offsetX == 0f && offsetY == 0f
}
