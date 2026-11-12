package app.v2c.ime.recorder

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PcmLevelTest {
    @Test
    fun silenceIsZeroAndLoudIsNearOne() {
        assertEquals(0f, PcmLevel.of(ShortArray(100), 100), 0f)
        assertTrue(PcmLevel.of(ShortArray(100) { 20000 }, 100) > 0.9f)
    }

    @Test
    fun emptyBlockIsZeroAndOnlyCountSamplesAreUsed() {
        assertEquals(0f, PcmLevel.of(ShortArray(0), 0), 0f)
        val buf = ShortArray(100) { if (it < 10) 0 else 30000 }
        assertEquals(0f, PcmLevel.of(buf, 10), 0f)
    }
}
