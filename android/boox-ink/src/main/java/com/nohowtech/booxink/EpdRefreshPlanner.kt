package com.nohowtech.booxink

/**
 * Decides how much of an ePaper screen a change has to refresh.
 *
 * A full GC refresh clears ghosting but flashes the whole panel, so it is kept for changes that
 * touch most of the screen. Smaller changes, such as erased strokes and undo, refresh only their
 * own rectangle. Regional refreshes leave ghosts that build up, so every [fullEvery]th one is
 * promoted to a full refresh.
 */
class EpdRefreshPlanner(
    private val fullEvery: Int = 30,
    private val fullAreaFraction: Float = 0.5f,
) {
    enum class Kind { REGIONAL, FULL }

    private var sinceFull = 0

    /** A change covering [changedFraction] of the view (0..1). Call once for each change. */
    fun regional(changedFraction: Float): Kind {
        sinceFull++
        if (changedFraction >= fullAreaFraction || sinceFull >= fullEvery) {
            sinceFull = 0
            return Kind.FULL
        }
        return Kind.REGIONAL
    }

    /** A full refresh happened for another reason, so the ghosts are gone. */
    fun fullDone() { sinceFull = 0 }

    companion object {
        /** Grows document-space bounds [a] to include [b]. */
        fun union(a: FloatArray?, b: FloatArray): FloatArray =
            if (a == null) b.copyOf() else floatArrayOf(minOf(a[0], b[0]), minOf(a[1], b[1]), maxOf(a[2], b[2]), maxOf(a[3], b[3]))
    }
}
