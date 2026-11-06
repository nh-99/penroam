package com.nohowtech.booxink

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class EpdRefreshPlannerTest {
    @Test fun `small changes stay regional`() {
        val p = EpdRefreshPlanner(fullEvery = 5)
        repeat(4) { assertEquals(EpdRefreshPlanner.Kind.REGIONAL, p.regional(0.02f)) }
    }

    @Test fun `ghosts are cleared by a periodic full refresh`() {
        val p = EpdRefreshPlanner(fullEvery = 3)
        p.regional(0.02f); p.regional(0.02f)
        assertEquals(EpdRefreshPlanner.Kind.FULL, p.regional(0.02f))
        assertEquals(EpdRefreshPlanner.Kind.REGIONAL, p.regional(0.02f))
    }

    @Test fun `a change over most of the screen is full and restarts the count`() {
        val p = EpdRefreshPlanner(fullEvery = 3)
        p.regional(0.02f)
        assertEquals(EpdRefreshPlanner.Kind.FULL, p.regional(0.6f))
        assertEquals(EpdRefreshPlanner.Kind.REGIONAL, p.regional(0.02f))
        assertEquals(EpdRefreshPlanner.Kind.REGIONAL, p.regional(0.02f))
    }

    @Test fun `an unrelated full refresh restarts the count`() {
        val p = EpdRefreshPlanner(fullEvery = 3)
        p.regional(0.02f); p.regional(0.02f)
        p.fullDone()
        assertEquals(EpdRefreshPlanner.Kind.REGIONAL, p.regional(0.02f))
    }

    @Test fun `bounds union`() {
        val u = EpdRefreshPlanner.union(floatArrayOf(1f, 2f, 3f, 4f), floatArrayOf(0f, 3f, 5f, 3.5f))
        assertArrayEquals(floatArrayOf(0f, 2f, 5f, 4f), u, 0f)
        assertArrayEquals(floatArrayOf(1f, 1f, 2f, 2f), EpdRefreshPlanner.union(null, floatArrayOf(1f, 1f, 2f, 2f)), 0f)
    }
}
