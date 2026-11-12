package app.v2c.ime

import org.junit.Assert.assertEquals
import org.junit.Test

class FormatElapsedTest {
    @Test
    fun formatsMinutesAndSeconds() {
        assertEquals("0:00", formatElapsed(0))
        assertEquals("0:09", formatElapsed(9_999))
        assertEquals("1:05", formatElapsed(65_000))
        assertEquals("0:00", formatElapsed(-5))
    }
}
