package app.v2c.ime.recorder

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Handler
import android.os.Looper
import androidx.core.content.ContextCompat

/**
 * [PcmRecorder] on [AudioRecord] (16 kHz mono PCM16). Android glue, not unit tested.
 * RECORD_AUDIO is granted by the setup activity; it is checked again here.
 */
class AudioRecordPcmRecorder(private val context: Context) : PcmRecorder {
    private val main = Handler(Looper.getMainLooper())
    private val lock = Any()
    private var record: AudioRecord? = null
    private var thread: Thread? = null
    private var samples = ShortArray(0)
    private var count = 0

    @Volatile private var running = false

    @SuppressLint("MissingPermission") // Checked with ContextCompat just below.
    override fun start(onLevel: (Float) -> Unit): Boolean {
        if (running) return false
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            return false
        }
        val minBuf = AudioRecord.getMinBufferSize(
            PcmRecorder.SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        if (minBuf <= 0) return false
        val readSize = PcmRecorder.SAMPLE_RATE / 10 // 100 ms per read
        val rec = try {
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_RECOGNITION,
                PcmRecorder.SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                maxOf(minBuf, readSize * 2 * 2),
            )
        } catch (_: IllegalArgumentException) {
            return false
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            rec.release()
            return false
        }
        synchronized(lock) {
            samples = ShortArray(PcmRecorder.SAMPLE_RATE * 10)
            count = 0
        }
        try {
            rec.startRecording()
        } catch (_: IllegalStateException) {
            rec.release()
            return false
        }
        record = rec
        running = true
        thread = Thread({
            val buf = ShortArray(readSize)
            val max = PcmRecorder.SAMPLE_RATE * PcmRecorder.MAX_SECONDS
            while (running) {
                val n = rec.read(buf, 0, buf.size)
                if (n <= 0) {
                    if (n < 0) break // ERROR_*: stop capturing, keep what we have
                    continue
                }
                synchronized(lock) {
                    val room = max - count
                    val take = minOf(n, room)
                    if (take > 0) {
                        if (count + take > samples.size) {
                            samples = samples.copyOf(maxOf(samples.size * 2, count + take))
                        }
                        System.arraycopy(buf, 0, samples, count, take)
                        count += take
                    }
                }
                val level = PcmLevel.of(buf, n)
                main.post { onLevel(level) }
            }
        }, "v2c-pcm-record").also { it.start() }
        return true
    }

    override fun stop(): ShortArray {
        halt()
        return synchronized(lock) { samples.copyOf(count).also { samples = ShortArray(0); count = 0 } }
    }

    override fun cancel() {
        halt()
        synchronized(lock) { samples = ShortArray(0); count = 0 }
    }

    private fun halt() {
        running = false
        thread?.let {
            try {
                it.join(500)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
        thread = null
        record?.let {
            try {
                it.stop()
            } catch (_: IllegalStateException) {
                // Not recording any more.
            }
            it.release()
        }
        record = null
    }
}
