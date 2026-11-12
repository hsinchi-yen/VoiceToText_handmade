package app.v2c.ime.asr

/**
 * Splits long recordings so no piece exceeds what Whisper-style models accept (30 s) and
 * converts PCM for the FFI. Pure, unit tested.
 */
object PcmChunker {
    const val SAMPLE_RATE = 16_000
    const val MAX_CHUNK_SECONDS = 30
    private const val SEARCH_WINDOW_SECONDS = 3
    private const val FRAME_MS = 20

    /**
     * Pieces of at most [maxSeconds]. A cut is placed at the quietest 20 ms frame inside the last
     * 3 s before the limit so words are rarely split. Concatenating the pieces gives back [pcm].
     */
    fun split(pcm: ShortArray, rate: Int = SAMPLE_RATE, maxSeconds: Int = MAX_CHUNK_SECONDS): List<ShortArray> {
        val maxLen = rate * maxSeconds
        if (pcm.size <= maxLen) return listOf(pcm)
        val frame = rate * FRAME_MS / 1000
        val window = minOf(rate * SEARCH_WINDOW_SECONDS, maxLen / 2)
        val out = ArrayList<ShortArray>()
        var start = 0
        while (pcm.size - start > maxLen) {
            val limit = start + maxLen
            var bestCut = limit
            var bestEnergy = Long.MAX_VALUE
            var frameStart = limit - window
            while (frameStart + frame <= limit) {
                var e = 0L
                for (i in frameStart until frameStart + frame) e += Math.abs(pcm[i].toInt())
                if (e < bestEnergy) {
                    bestEnergy = e
                    bestCut = frameStart
                }
                frameStart += frame
            }
            out += pcm.copyOfRange(start, bestCut)
            start = bestCut
        }
        out += pcm.copyOfRange(start, pcm.size)
        return out
    }

    /** 16-bit little-endian bytes, the format `transcribe_pcm` expects. */
    fun toLittleEndianBytes(pcm: ShortArray): ByteArray {
        val out = ByteArray(pcm.size * 2)
        for (i in pcm.indices) {
            val v = pcm[i].toInt()
            out[2 * i] = (v and 0xFF).toByte()
            out[2 * i + 1] = ((v shr 8) and 0xFF).toByte()
        }
        return out
    }
}
