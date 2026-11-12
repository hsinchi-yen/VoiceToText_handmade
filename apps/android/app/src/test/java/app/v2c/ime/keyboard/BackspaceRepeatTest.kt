package app.v2c.ime.keyboard

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class BackspaceRepeatTest {
    private val r = BackspaceRepeat()

    @Test fun firstRepeatUsesInitialDelay() = assertEquals(400L, r.delayBeforeRepeat(0))

    @Test fun earlyRepeatsUseStartInterval() {
        for (i in 1..5) assertEquals(100L, r.delayBeforeRepeat(i))
    }

    @Test fun intervalShrinksThenClamps() {
        assertEquals(70L, r.delayBeforeRepeat(6))
        assertTrue(r.delayBeforeRepeat(11) < r.delayBeforeRepeat(6))
        assertEquals(30L, r.delayBeforeRepeat(1000))
    }

    @Test fun neverIncreases() {
        var prev = Long.MAX_VALUE
        for (i in 1..200) {
            val d = r.delayBeforeRepeat(i)
            assertTrue(d <= prev)
            prev = d
        }
    }
}
