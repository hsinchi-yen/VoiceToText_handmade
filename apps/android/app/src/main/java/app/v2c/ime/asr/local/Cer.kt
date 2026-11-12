package app.v2c.ime.asr.local

import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Character error rate used by the comparison tool (numbers only, no text kept). */
object Cer {
    /** Lowercases and drops whitespace and punctuation so only the spoken characters count. */
    fun normalize(s: String): String =
        s.lowercase().filter { Character.isLetterOrDigit(it) }

    /** Levenshtein distance over UTF-16 code points. */
    fun editDistance(a: IntArray, b: IntArray): Int {
        if (a.isEmpty()) return b.size
        if (b.isEmpty()) return a.size
        var prev = IntArray(b.size + 1) { it }
        var cur = IntArray(b.size + 1)
        for (i in 1..a.size) {
            cur[0] = i
            for (j in 1..b.size) {
                val sub = prev[j - 1] + if (a[i - 1] == b[j - 1]) 0 else 1
                cur[j] = minOf(sub, prev[j] + 1, cur[j - 1] + 1)
            }
            val t = prev
            prev = cur
            cur = t
        }
        return prev[b.size]
    }

    /** Errors and reference length after normalisation. */
    data class Score(val errors: Int, val referenceLength: Int) {
        val rate: Double get() = if (referenceLength == 0) 0.0 else errors.toDouble() / referenceLength
    }

    fun score(reference: String, hypothesis: String): Score {
        val ref = normalize(reference).codePoints().toArray()
        val hyp = normalize(hypothesis).codePoints().toArray()
        return Score(editDistance(ref, hyp), ref.size)
    }

    /** Corpus-level CER: total errors over total reference characters. */
    fun aggregate(scores: List<Score>): Double {
        val len = scores.sumOf { it.referenceLength }
        return if (len == 0) 0.0 else scores.sumOf { it.errors }.toDouble() / len
    }
}

/** Minimal RIFF/WAVE reader for 16-bit PCM test recordings. */
object WavReader {
    class Wav(val pcm: ShortArray, val rate: Int) {
        val seconds: Double get() = pcm.size.toDouble() / rate
    }

    fun read(file: File): Wav = parse(file.readBytes())

    /** Parses 16-bit PCM; multi-channel audio is reduced to its first channel. */
    fun parse(bytes: ByteArray): Wav {
        require(bytes.size >= 12 && String(bytes, 0, 4, Charsets.US_ASCII) == "RIFF") { "not a RIFF file" }
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        var pos = 12
        var rate = 0
        var channels = 1
        var bits = 0
        var format = 0
        while (pos + 8 <= bytes.size) {
            val id = String(bytes, pos, 4, Charsets.US_ASCII)
            val size = buf.getInt(pos + 4).toLong() and 0xFFFFFFFFL
            val body = pos + 8
            if (id == "fmt ") {
                format = buf.getShort(body).toInt() and 0xFFFF
                channels = buf.getShort(body + 2).toInt() and 0xFFFF
                rate = buf.getInt(body + 4)
                bits = buf.getShort(body + 14).toInt() and 0xFFFF
            } else if (id == "data") {
                require((format == 1 || format == 0xFFFE) && bits == 16 && channels >= 1 && rate > 0) {
                    "only 16-bit PCM WAV is supported"
                }
                val available = minOf(size, (bytes.size - body).toLong()).toInt()
                val frames = available / (2 * channels)
                val pcm = ShortArray(frames) { buf.getShort(body + it * 2 * channels) }
                return Wav(pcm, rate)
            }
            pos = body + size.toInt() + (size.toInt() and 1)
        }
        throw IllegalArgumentException("no data chunk")
    }

    /** Test helper: builds a mono 16-bit WAV in memory. */
    fun encode(pcm: ShortArray, rate: Int): ByteArray {
        val data = pcm.size * 2
        val b = ByteBuffer.allocate(44 + data).order(ByteOrder.LITTLE_ENDIAN)
        b.put("RIFF".toByteArray()).putInt(36 + data).put("WAVE".toByteArray())
        b.put("fmt ".toByteArray()).putInt(16).putShort(1).putShort(1).putInt(rate)
            .putInt(rate * 2).putShort(2).putShort(16)
        b.put("data".toByteArray()).putInt(data)
        pcm.forEach { b.putShort(it) }
        return b.array()
    }
}
