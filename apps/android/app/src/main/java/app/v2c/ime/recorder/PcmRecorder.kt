package app.v2c.ime.recorder

/**
 * Records 16 kHz mono 16-bit PCM for the LAN and local-model sources (the system recognizer
 * records by itself). Implementation: [AudioRecordPcmRecorder]; tests use fakes.
 */
interface PcmRecorder {
    /** Starts capturing. [onLevel] gets 0..1 levels on the main thread. False when the mic is unavailable. */
    fun start(onLevel: (Float) -> Unit): Boolean

    /** Stops and returns everything captured since [start] (empty when not recording). */
    fun stop(): ShortArray

    /** Stops and drops the audio. */
    fun cancel()

    companion object {
        const val SAMPLE_RATE = 16_000

        /** Recording stops growing here (about 5.5 MB of PCM); plenty for one dictated sentence. */
        const val MAX_SECONDS = 180
    }
}

/** Pure helper for the waveform: normalized level (0..1) of a PCM block. */
object PcmLevel {
    fun of(buf: ShortArray, count: Int): Float {
        if (count <= 0) return 0f
        var sum = 0.0
        for (i in 0 until count) {
            val v = buf[i] / 32768.0
            sum += v * v
        }
        val rms = Math.sqrt(sum / count)
        // Speech is typically -40..-10 dBFS; map that range to 0..1.
        val db = 20.0 * Math.log10(rms.coerceAtLeast(1e-6))
        return ((db + 45.0) / 35.0).coerceIn(0.0, 1.0).toFloat()
    }
}
