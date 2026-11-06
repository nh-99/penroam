package com.nohowtech.booxink

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/** The fixture is shared with the web and Swift tests, so all platforms place ink identically. */
class ViewportVectorsTest {
    private fun fixture(): File = generateSequence(File("").absoluteFile) { it.parentFile }
        .map { File(it, "tests/fixtures/viewport-vectors.json") }
        .first { it.exists() }

    @Test fun `document points land on the shared pixels and come back`() {
        val cases = JSONObject(fixture().readText()).getJSONArray("cases")
        assertTrue(cases.length() > 0)
        for (i in 0 until cases.length()) {
            val c = cases.getJSONObject(i)
            val t = ViewTransform(c.getDouble("scale").toFloat(), c.getDouble("ox").toFloat(), c.getDouble("oy").toFloat(), c.getDouble("dpr").toFloat())
            val doc = c.getJSONArray("doc")
            val view = c.getJSONArray("view")
            // Float precision: within a hundredth of a pixel on a page this size
            assertEquals(view.getDouble(0), t.viewX(doc.getDouble(0).toFloat()).toDouble(), 0.02)
            assertEquals(view.getDouble(1), t.viewY(doc.getDouble(1).toFloat()).toDouble(), 0.02)
            assertEquals(doc.getDouble(0), t.docX(view.getDouble(0).toFloat()).toDouble(), 0.02)
            assertEquals(doc.getDouble(1), t.docY(view.getDouble(1).toFloat()).toDouble(), 0.02)
        }
    }
}
