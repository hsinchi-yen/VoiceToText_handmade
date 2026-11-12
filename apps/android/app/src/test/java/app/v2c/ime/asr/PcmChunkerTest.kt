package app.v2c.ime.asr

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PcmChunkerTest {
    private val rate = 16_000

    @Test
    fun shortAudioIsOneChunk() {
        val pcm = ShortArray(rate * 30) { 5 }
        assertEquals(1, PcmChunker.split(pcm).size)
        assertEquals(1, PcmChunker.split(ShortArray(0)).size)
    }

    @Test
    fun longAudioIsSplitIntoPiecesOfAtMost30SecondsAndNothingIsLost() {
        val pcm = ShortArray(rate * 75) { (it % 1000).toShort() }
        val chunks = PcmChunker.split(pcm)
        assertTrue(chunks.size in 3..4)
        assertTrue(chunks.all { it.size <= rate * 30 })
        assertArrayEquals(pcm, chunks.flatMap { it.asIterable() }.toShortArray())
    }

    @Test
    fun cutsAtTheQuietestFrameNearTheLimit() {
        // Loud everywhere except a silent 20 ms frame 1.5 s before the 30 s limit.
        val pcm = ShortArray(rate * 40) { 1000 }
        val silentStart = rate * 30 - rate * 3 / 2
        for (i in silentStart until silentStart + rate / 50) pcm[i] = 0
        val chunks = PcmChunker.split(pcm)
        assertEquals(silentStart, chunks[0].size)
    }

    @Test
    fun customLimitIsHonoured() {
        val chunks = PcmChunker.split(ShortArray(rate * 25) { 3 }, maxSeconds = 10)
        assertTrue(chunks.size >= 3)
        assertTrue(chunks.all { it.size <= rate * 10 })
    }

    @Test
    fun littleEndianBytes() {
        val bytes = PcmChunker.toLittleEndianBytes(shortArrayOf(0x0102, -2, 0))
        assertArrayEquals(byteArrayOf(0x02, 0x01, 0xFE.toByte(), 0xFF.toByte(), 0, 0), bytes)
    }
}
