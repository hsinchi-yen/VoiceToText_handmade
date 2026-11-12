package app.v2c.ime.asr.local

import java.io.Closeable
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.io.RandomAccessFile
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executor
import java.util.concurrent.Executors

/** Thrown by a [ModelFetcher] for an HTTP answer that retrying will not fix (404, 403, ...). */
class BadResponseException(message: String) : IOException(message)

/**
 * An open download. [resumed] is true when the server honoured the requested start offset
 * (HTTP 206); when false the body starts at byte 0 and the caller must restart the file.
 */
class FetchResponse(val stream: InputStream, val resumed: Boolean) : Closeable {
    override fun close() = stream.close()
}

/** Network seam so the state machine can be tested without a network. */
fun interface ModelFetcher {
    @Throws(IOException::class)
    fun open(url: String, offset: Long): FetchResponse
}

/** Real fetcher on `HttpURLConnection` (follows the Hugging Face to CDN redirect). */
class HttpModelFetcher(
    private val connectTimeoutMs: Int = 15_000,
    private val readTimeoutMs: Int = 30_000,
) : ModelFetcher {
    override fun open(url: String, offset: Long): FetchResponse {
        val conn = URL(url).openConnection() as HttpURLConnection
        conn.connectTimeout = connectTimeoutMs
        conn.readTimeout = readTimeoutMs
        conn.instanceFollowRedirects = true
        if (offset > 0) conn.setRequestProperty("Range", "bytes=$offset-")
        try {
            val code = conn.responseCode
            if (code != 200 && code != 206) throw BadResponseException("HTTP $code")
            if (code == 206 && contentRangeStart(conn.getHeaderField("Content-Range")) != offset) {
                throw BadResponseException("unexpected Content-Range")
            }
            return FetchResponse(conn.inputStream, resumed = offset > 0 && code == 206)
        } catch (e: Throwable) {
            conn.disconnect()
            throw e
        }
    }

    companion object {
        private val RANGE_START = Regex("""bytes\s+(\d+)-""")

        /** Start offset of a `bytes START-END/TOTAL` header, or -1 when missing or malformed. */
        fun contentRangeStart(header: String?): Long =
            header?.let { RANGE_START.find(it)?.groupValues?.get(1)?.toLongOrNull() } ?: -1L
    }
}

/**
 * Downloads one [LocalModelSpec] into `rootDir/<spec.id>/` (app-private storage).
 *
 * Rules:
 * - Bytes go to `<name>.part`; the file is renamed to its final name only after its SHA-256
 *   matches, so a final-named file is always verified. A mismatch deletes the partial file.
 * - A dropped connection leaves the `.part` file; [start] again resumes from its length
 *   (or restarts when the server ignores Range).
 * - Not enough free space or a write error is [DownloadFailure.STORAGE]; a read or connect
 *   error is [DownloadFailure.INTERRUPTED].
 * - Progress and state are exposed through [state] and [addListener]; state changes may arrive
 *   on the executor thread.
 * - [start], [cancel] and [delete] return immediately and are safe on the main thread; the
 *   work runs on [executor]. [download] is the blocking core (tests; not for the main thread).
 */
class LocalModelDownloader(
    val spec: LocalModelSpec,
    rootDir: File,
    private val fetcher: ModelFetcher = HttpModelFetcher(),
    private val executor: Executor = Executors.newSingleThreadExecutor { r ->
        Thread(r, "v2c-model-download").apply { isDaemon = true }
    },
    private val freeSpaceMarginBytes: Long = 64L * 1024 * 1024,
) {
    val modelDir: File = File(rootDir, spec.id)

    private val listeners = CopyOnWriteArrayList<(DownloadState) -> Unit>()
    private val lock = Any()
    private var running = false
    private var startPending = false

    @Volatile private var cancelRequested = false
    @Volatile private var deleteRequested = false

    @Volatile
    var state: DownloadState = if (isInstalled()) DownloadState.Ready else DownloadState.NotDownloaded
        private set

    fun addListener(l: (DownloadState) -> Unit) {
        listeners.add(l)
    }

    fun removeListener(l: (DownloadState) -> Unit) {
        listeners.remove(l)
    }

    fun isReady(): Boolean = state == DownloadState.Ready

    /** Absolute path of a verified model file. Only meaningful when [isReady]. */
    fun path(name: String): File = File(modelDir, name)

    /**
     * Starts (or resumes) the download in the background. No-op when ready. While a run is still
     * finishing (for example right after [cancel]) the start is remembered and begins as soon as
     * that run ends.
     */
    fun start() {
        synchronized(lock) {
            if (state == DownloadState.Ready) return
            // Reset here, not on the executor thread: a cancel() issued right after start()
            // must not be overwritten when the queued task finally runs.
            if (!deleteRequested) cancelRequested = false
            if (running) {
                startPending = true
                return
            }
            running = true
        }
        executor.execute { run() }
    }

    /** Stops a running download; partial data is kept so [start] can resume. Also drops a pending start. */
    fun cancel() {
        synchronized(lock) {
            cancelRequested = true
            startPending = false
        }
    }

    /** Removes the model files (also partial ones), stopping a running download first. Non-blocking. */
    fun delete() {
        synchronized(lock) {
            deleteRequested = true
            cancelRequested = true
            startPending = false
            if (running) return
            running = true
        }
        executor.execute { run() }
    }

    /** Blocking download on the calling thread. Returns the final state. */
    fun download(): DownloadState {
        synchronized(lock) {
            if (running) return state
            running = true
            cancelRequested = false
            deleteRequested = false
        }
        run()
        return state
    }

    private fun takeDeleteRequest(): Boolean = synchronized(lock) {
        val d = deleteRequested
        deleteRequested = false
        d
    }

    private fun run() {
        try {
            if (takeDeleteRequest()) {
                wipe()
                return
            }
            synchronized(lock) { startPending = false }
            val result = try {
                downloadAll()
            } catch (e: IOException) {
                DownloadState.Failed(DownloadFailure.STORAGE)
            } catch (e: Exception) {
                DownloadState.Failed(DownloadFailure.INTERRUPTED)
            }
            if (takeDeleteRequest()) wipe() else publish(result)
        } finally {
            // A delete() or start() that arrived while this run was ending is not lost: run again.
            val again = synchronized(lock) {
                if (deleteRequested || startPending) {
                    true
                } else {
                    running = false
                    false
                }
            }
            if (again) executor.execute { run() }
        }
    }

    private fun downloadAll(): DownloadState {
        if (cancelRequested) return DownloadState.NotDownloaded
        if (!modelDir.isDirectory && !modelDir.mkdirs()) return DownloadState.Failed(DownloadFailure.STORAGE)
        val total = spec.totalBytes
        val remaining = spec.files.sumOf { f ->
            val dest = File(modelDir, f.name)
            if (dest.isFile && dest.length() == f.sizeBytes) {
                0L
            } else {
                val part = File(modelDir, f.name + ".part")
                val have = if (part.isFile && part.length() <= f.sizeBytes) part.length() else 0L
                f.sizeBytes - have
            }
        }
        if (remaining > 0 && modelDir.usableSpace < remaining + freeSpaceMarginBytes) {
            return DownloadState.Failed(DownloadFailure.STORAGE)
        }
        var finishedBytes = 0L
        publish(DownloadState.Downloading(0, total))
        for (file in spec.files) {
            val dest = File(modelDir, file.name)
            if (dest.isFile && dest.length() == file.sizeBytes) {
                finishedBytes += file.sizeBytes
                continue
            }
            val outcome = fetchFile(file, finishedBytes, total)
            if (outcome != null) return outcome
            finishedBytes += file.sizeBytes
        }
        return DownloadState.Ready
    }

    /** Returns null on success or the terminal state to stop with. */
    private fun fetchFile(file: ModelFile, doneBefore: Long, total: Long): DownloadState? {
        val part = File(modelDir, file.name + ".part")
        var offset = if (part.isFile) part.length() else 0L
        if (offset > file.sizeBytes) {
            part.delete()
            offset = 0
        }
        if (offset < file.sizeBytes) {
            val failure = try {
                copyInto(file, part, offset, doneBefore, total)
            } catch (e: BadResponseException) {
                return DownloadState.Failed(DownloadFailure.BAD_RESPONSE)
            } catch (e: ReadFailure) {
                return DownloadState.Failed(DownloadFailure.INTERRUPTED)
            } catch (e: IOException) {
                return DownloadState.Failed(DownloadFailure.STORAGE)
            }
            if (cancelRequested) return DownloadState.NotDownloaded
            if (failure != null) {
                if (failure.reason == DownloadFailure.BAD_RESPONSE) part.delete()
                return failure
            }
        }
        if (cancelRequested) return DownloadState.NotDownloaded
        publish(DownloadState.Verifying)
        if (part.length() != file.sizeBytes) {
            return DownloadState.Failed(DownloadFailure.INTERRUPTED)
        }
        if (!sha256Of(part).equals(file.sha256, ignoreCase = true)) {
            part.delete()
            return DownloadState.Failed(DownloadFailure.HASH_MISMATCH)
        }
        val dest = File(modelDir, file.name)
        dest.delete()
        if (!part.renameTo(dest)) return DownloadState.Failed(DownloadFailure.STORAGE)
        return null
    }

    /** A network-side IOException, kept apart from disk write errors. */
    private class ReadFailure(cause: IOException) : IOException(cause)

    /** Appends the response to [part]. Returns a failure state, or null when the stream ended. */
    private fun copyInto(
        file: ModelFile,
        part: File,
        offset: Long,
        doneBefore: Long,
        total: Long,
    ): DownloadState.Failed? {
        if (cancelRequested) return null // caller maps a set cancel flag to NotDownloaded
        val response = try {
            fetcher.open(file.url, offset)
        } catch (e: BadResponseException) {
            throw e
        } catch (e: IOException) {
            throw ReadFailure(e)
        }
        try {
            val start = if (offset > 0 && !response.resumed) 0L else offset
            RandomAccessFile(part, "rw").use { out ->
                out.setLength(start)
                out.seek(start)
                val buf = ByteArray(BUFFER_SIZE)
                var written = start
                var lastReport = written
                while (!cancelRequested) {
                    val n = try {
                        response.stream.read(buf)
                    } catch (e: BadResponseException) {
                        throw e
                    } catch (e: IOException) {
                        throw ReadFailure(e)
                    }
                    if (n < 0) break
                    if (written + n > file.sizeBytes) return DownloadState.Failed(DownloadFailure.BAD_RESPONSE)
                    out.write(buf, 0, n)
                    written += n
                    if (written - lastReport >= REPORT_STEP) {
                        lastReport = written
                        publish(DownloadState.Downloading(doneBefore + written, total))
                    }
                }
            }
        } finally {
            // A failing close() is network-side noise: ignore it, never report it as STORAGE.
            try {
                response.close()
            } catch (_: IOException) {
            }
        }
        return null
    }

    private fun isInstalled(): Boolean = spec.files.all {
        val f = File(modelDir, it.name)
        f.isFile && f.length() == it.sizeBytes
    }

    private fun wipe() {
        modelDir.deleteRecursively()
        publish(DownloadState.NotDownloaded)
    }

    private fun publish(s: DownloadState) {
        state = s
        for (l in listeners) l(s)
    }

    companion object {
        private const val BUFFER_SIZE = 64 * 1024
        private const val REPORT_STEP = 256 * 1024

        fun sha256Of(file: File): String {
            val md = MessageDigest.getInstance("SHA-256")
            file.inputStream().use { input ->
                val buf = ByteArray(BUFFER_SIZE)
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    md.update(buf, 0, n)
                }
            }
            return md.digest().joinToString("") { "%02x".format(it) }
        }
    }
}
