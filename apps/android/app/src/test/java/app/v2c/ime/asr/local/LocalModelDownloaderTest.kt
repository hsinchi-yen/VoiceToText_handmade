package app.v2c.ime.asr.local

import java.io.ByteArrayInputStream
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.security.MessageDigest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class LocalModelDownloaderTest {
    @get:Rule val tmp = TemporaryFolder()

    private val blobA = ByteArray(700_000) { (it * 31 + 7).toByte() }
    private val blobB = ByteArray(1_000) { (it * 5).toByte() }
    private lateinit var spec: LocalModelSpec

    private fun sha(b: ByteArray) =
        MessageDigest.getInstance("SHA-256").digest(b).joinToString("") { "%02x".format(it) }

    @Before
    fun setUp() {
        spec = LocalModelSpec(
            id = "t",
            displayName = "t",
            family = ModelFamily.SENSE_VOICE,
            files = listOf(
                ModelFile("a.bin", "mem://a", blobA.size.toLong(), sha(blobA)),
                ModelFile("b.txt", "mem://b", blobB.size.toLong(), sha(blobB)),
            ),
        )
    }

    private val directExecutor = java.util.concurrent.Executor { it.run() }

    /** Serves blobs; [failAfter] makes the first open of a.bin die after that many bytes. */
    private inner class FakeFetcher(
        var failAfter: Int = -1,
        val honorRange: Boolean = true,
        val corrupt: Boolean = false,
    ) : ModelFetcher {
        val offsets = mutableListOf<Long>()

        override fun open(url: String, offset: Long): FetchResponse {
            offsets += offset
            var data = if (url.endsWith("a")) blobA else blobB
            if (corrupt) data = data.copyOf().also { it[0] = (it[0] + 1).toByte() }
            val resumed = honorRange && offset > 0
            val start = if (resumed) offset.toInt() else 0
            val body = data.copyOfRange(start, data.size)
            val limit = failAfter
            val stream: InputStream =
                if (limit >= 0 && url.endsWith("a")) {
                    failAfter = -1
                    object : InputStream() {
                        val inner = ByteArrayInputStream(body)
                        var left = limit
                        override fun read(): Int = throw UnsupportedOperationException()
                        override fun read(b: ByteArray, off: Int, len: Int): Int {
                            if (left <= 0) throw IOException("connection reset")
                            val n = inner.read(b, off, minOf(len, left))
                            if (n > 0) left -= n
                            return n
                        }
                    }
                } else {
                    ByteArrayInputStream(body)
                }
            return FetchResponse(stream, resumed)
        }
    }

    private fun downloader(f: ModelFetcher) = LocalModelDownloader(spec, tmp.root, f, directExecutor)

    @Test
    fun completeDownloadIsVerifiedAndReady() {
        val d = downloader(FakeFetcher())
        assertEquals(DownloadState.NotDownloaded, d.state)
        val seen = mutableListOf<DownloadState>()
        d.addListener { seen += it }

        assertEquals(DownloadState.Ready, d.download())

        assertTrue(d.isReady())
        assertTrue(d.path("a.bin").readBytes().contentEquals(blobA))
        assertTrue(d.path("b.txt").readBytes().contentEquals(blobB))
        assertFalse(File(d.modelDir, "a.bin.part").exists())
        assertTrue(seen.any { it is DownloadState.Downloading })
        assertTrue(seen.contains(DownloadState.Verifying))
        // A new instance on the same directory sees the installed model.
        assertEquals(DownloadState.Ready, downloader(FakeFetcher()).state)
    }

    @Test
    fun interruptedDownloadKeepsPartAndResumes() {
        val fetcher = FakeFetcher(failAfter = 300_000)
        val d = downloader(fetcher)

        assertEquals(DownloadState.Failed(DownloadFailure.INTERRUPTED), d.download())
        assertFalse(d.isReady())
        assertFalse(d.path("a.bin").exists())
        val part = File(d.modelDir, "a.bin.part")
        assertEquals(300_000L, part.length())

        assertEquals(DownloadState.Ready, d.download())
        assertEquals(300_000L, fetcher.offsets[1]) // resumed with a Range offset, not from zero
        assertTrue(d.path("a.bin").readBytes().contentEquals(blobA))
    }

    @Test
    fun serverIgnoringRangeRestartsTheFile() {
        val fetcher = FakeFetcher(failAfter = 100_000, honorRange = false)
        val d = downloader(fetcher)
        assertEquals(DownloadState.Failed(DownloadFailure.INTERRUPTED), d.download())

        assertEquals(DownloadState.Ready, d.download())
        assertTrue(d.path("a.bin").readBytes().contentEquals(blobA))
    }

    @Test
    fun hashMismatchIsRejectedAndNeverUsed() {
        val d = downloader(FakeFetcher(corrupt = true))

        assertEquals(DownloadState.Failed(DownloadFailure.HASH_MISMATCH), d.download())

        assertFalse(d.isReady())
        assertFalse(d.path("a.bin").exists())
        assertFalse(File(d.modelDir, "a.bin.part").exists())
        // A fresh instance does not consider it installed either.
        assertEquals(DownloadState.NotDownloaded, downloader(FakeFetcher()).state)
    }

    @Test
    fun badHttpStatusIsReported() {
        val d = downloader { _, _ -> throw BadResponseException("HTTP 404") }
        assertEquals(DownloadState.Failed(DownloadFailure.BAD_RESPONSE), d.download())
    }

    @Test
    fun cancelKeepsPartialDataAndReturnsToNotDownloaded() {
        lateinit var d: LocalModelDownloader
        val fetcher = ModelFetcher { _, _ ->
            val stream = object : InputStream() {
                var sent = 0
                override fun read(): Int = throw UnsupportedOperationException()
                override fun read(b: ByteArray, off: Int, len: Int): Int {
                    if (sent >= 100_000) d.cancel()
                    val n = minOf(len, 50_000)
                    sent += n
                    return n
                }
            }
            FetchResponse(stream, false)
        }
        d = downloader(fetcher)

        assertEquals(DownloadState.NotDownloaded, d.download())
        assertTrue(File(d.modelDir, "a.bin.part").exists())
        assertFalse(d.isReady())
    }

    @Test
    fun deleteRemovesEverything() {
        val d = downloader(FakeFetcher())
        d.download()
        assertTrue(d.isReady())

        d.delete()

        assertEquals(DownloadState.NotDownloaded, d.state)
        assertFalse(d.modelDir.exists())
    }

    private val queue = ArrayDeque<Runnable>()
    private val queuedExecutor = java.util.concurrent.Executor { queue.add(it) }

    @Test
    fun cancelRightAfterStartIsNotLost() {
        val fetcher = FakeFetcher()
        val d = LocalModelDownloader(spec, tmp.root, fetcher, queuedExecutor)
        d.start()
        d.cancel()
        queue.removeFirst().run()

        assertEquals(DownloadState.NotDownloaded, d.state)
        assertFalse(d.isReady())
    }

    @Test
    fun deleteRightAfterStartReturnsAtOnceAndWipes() {
        val d = LocalModelDownloader(spec, tmp.root, FakeFetcher(), queuedExecutor)
        d.start()
        d.delete() // must not block or run on this thread
        assertEquals(1, queue.size)
        queue.removeFirst().run()

        assertEquals(DownloadState.NotDownloaded, d.state)
        assertFalse(d.modelDir.exists())
    }

    @Test
    fun deleteIsRunOnTheExecutor() {
        val d = LocalModelDownloader(spec, tmp.root, FakeFetcher(), queuedExecutor)
        d.download()
        d.delete()
        assertTrue(d.modelDir.exists()) // nothing happened on the caller thread
        queue.removeFirst().run()
        assertFalse(d.modelDir.exists())
    }

    @Test
    fun deleteDuringDownloadStopsAndRemovesFiles() {
        lateinit var d: LocalModelDownloader
        val fetcher = ModelFetcher { _, _ ->
            val stream = object : InputStream() {
                var sent = 0
                override fun read(): Int = throw UnsupportedOperationException()
                override fun read(b: ByteArray, off: Int, len: Int): Int {
                    if (sent >= 100_000) d.delete()
                    val n = minOf(len, 50_000)
                    sent += n
                    return n
                }
            }
            FetchResponse(stream, false)
        }
        d = downloader(fetcher)

        assertEquals(DownloadState.NotDownloaded, d.download())
        assertFalse(d.modelDir.exists())
    }

    @Test
    fun partLargerThanExpectedIsDiscarded() {
        val dir = File(tmp.root, spec.id).apply { mkdirs() }
        File(dir, "a.bin.part").writeBytes(ByteArray(blobA.size * 2))
        val d = downloader(FakeFetcher())

        assertEquals(DownloadState.Ready, d.download())
        assertTrue(d.path("a.bin").readBytes().contentEquals(blobA))
    }

    @Test
    fun serverSendingExtraBytesIsRejectedAndPartRemoved() {
        val d = downloader { _, _ -> FetchResponse(ByteArrayInputStream(blobA + ByteArray(10)), false) }

        assertEquals(DownloadState.Failed(DownloadFailure.BAD_RESPONSE), d.download())
        assertFalse(File(d.modelDir, "a.bin.part").exists())
        assertFalse(d.isReady())
    }

    @Test
    fun notEnoughFreeSpaceFailsBeforeDownloading() {
        val huge = spec.copy(files = listOf(ModelFile("a.bin", "mem://a", Long.MAX_VALUE / 4, sha(blobA))))
        var called = false
        val d = LocalModelDownloader(huge, tmp.root, { _, _ -> called = true; throw IOException() }, directExecutor)

        assertEquals(DownloadState.Failed(DownloadFailure.STORAGE), d.download())
        assertFalse(called)
    }

    @Test
    fun connectErrorIsInterruptedNotStorage() {
        val d = downloader { _, _ -> throw IOException("no route") }
        assertEquals(DownloadState.Failed(DownloadFailure.INTERRUPTED), d.download())
    }

    @Test
    fun unexpectedExceptionNeverLeavesStateDownloading() {
        val d = downloader { _, _ -> throw IllegalStateException("boom") }
        val s = d.download()
        assertTrue(s is DownloadState.Failed)
        assertEquals(s, d.state)
    }

    @Test
    fun contentRangeStartParsing() {
        assertEquals(300L, HttpModelFetcher.contentRangeStart("bytes 300-999/1000"))
        assertEquals(-1L, HttpModelFetcher.contentRangeStart(null))
        assertEquals(-1L, HttpModelFetcher.contentRangeStart("garbage"))
    }

    private fun serve(url: String, offset: Long): InputStream {
        val data = if (url.endsWith("a")) blobA else blobB
        return ByteArrayInputStream(data.copyOfRange(offset.toInt(), data.size))
    }

    @Test
    fun deleteArrivingWhileTheRunIsEndingIsNotLost() {
        lateinit var d: LocalModelDownloader
        var fired = false
        d = downloader(FakeFetcher())
        d.addListener {
            // Ready is published after the run took its delete flag but before it cleared `running`.
            if (it == DownloadState.Ready && !fired) {
                fired = true
                d.delete()
            }
        }

        d.download()

        assertEquals(DownloadState.NotDownloaded, d.state)
        assertFalse(d.modelDir.exists())
    }

    @Test
    fun cancelRightAfterStartNeverOpensTheConnection() {
        var opens = 0
        val d = LocalModelDownloader(spec, tmp.root, { _, _ -> opens++; throw IOException("offline") }, queuedExecutor)
        d.start()
        d.cancel()
        queue.removeFirst().run()

        assertEquals(0, opens)
        assertEquals(DownloadState.NotDownloaded, d.state)
    }

    @Test
    fun writeErrorIsStorageNotInterrupted() {
        val dir = File(tmp.root, spec.id).apply { mkdirs() }
        File(dir, "a.bin.part").mkdirs() // a directory where the partial file must go
        val d = downloader(FakeFetcher())

        assertEquals(DownloadState.Failed(DownloadFailure.STORAGE), d.download())
    }

    @Test
    fun failingCloseDoesNotCountAsStorageError() {
        val closeFails = ModelFetcher { url, offset ->
            val inner = serve(url, offset)
            FetchResponse(
                object : InputStream() {
                    override fun read(): Int = inner.read()
                    override fun read(b: ByteArray, off: Int, len: Int): Int = inner.read(b, off, len)
                    override fun close() {
                        throw IOException("close failed")
                    }
                },
                offset > 0,
            )
        }
        assertEquals(DownloadState.Ready, downloader(closeFails).download())

        val readAndCloseFail = ModelFetcher { _, _ ->
            FetchResponse(
                object : InputStream() {
                    override fun read(): Int = throw IOException("reset")
                    override fun read(b: ByteArray, off: Int, len: Int): Int = throw IOException("reset")
                    override fun close() {
                        throw IOException("close failed")
                    }
                },
                false,
            )
        }
        val d2 = LocalModelDownloader(spec.copy(id = "t2"), tmp.root, readAndCloseFail, directExecutor)
        assertEquals(DownloadState.Failed(DownloadFailure.INTERRUPTED), d2.download())
    }

    @Test
    fun startAfterCancelWhileRunIsEndingStillDownloads() {
        lateinit var d: LocalModelDownloader
        val offsets = mutableListOf<Long>()
        val fetcher = ModelFetcher { url, offset ->
            offsets += offset
            val inner = serve(url, offset)
            if (offsets.size == 1) {
                FetchResponse(
                    object : InputStream() {
                        override fun read(): Int = inner.read()
                        override fun read(b: ByteArray, off: Int, len: Int): Int {
                            d.cancel()
                            return inner.read(b, off, minOf(len, 65_536))
                        }
                    },
                    false,
                )
            } else {
                FetchResponse(inner, offset > 0)
            }
        }
        d = downloader(fetcher)
        // The run ends as NotDownloaded (cancelled); the user taps again before it fully finished.
        d.addListener { if (it == DownloadState.NotDownloaded) d.start() }

        d.download()

        assertEquals(DownloadState.Ready, d.state)
        assertTrue(offsets[1] > 0) // resumed from the kept partial file
        assertTrue(d.path("a.bin").readBytes().contentEquals(blobA))
    }

    @Test
    fun startDuringAHealthyDownloadIsANoOp() {
        val fetcher = FakeFetcher(corrupt = true)
        lateinit var d: LocalModelDownloader
        d = downloader(fetcher)
        var tapped = false
        d.addListener {
            // A second tap while the first run is still downloading.
            if (it is DownloadState.Downloading && !tapped) {
                tapped = true
                d.start()
            }
        }

        d.download()

        // No second run that would silently re-download after the hash mismatch.
        assertEquals(DownloadState.Failed(DownloadFailure.HASH_MISMATCH), d.state)
        assertEquals(1, fetcher.offsets.size)
    }

    @Test
    fun startAfterDeleteWhileTheRunIsEndingDownloadsAgain() {
        val d = LocalModelDownloader(spec, tmp.root, FakeFetcher(), queuedExecutor)
        d.start()
        queue.removeFirst().run() // first run completes
        assertEquals(DownloadState.Ready, d.state)

        d.delete()
        d.start() // the user taps download again before the delete ran
        while (queue.isNotEmpty()) queue.removeFirst().run()

        assertEquals(DownloadState.Ready, d.state)
        assertTrue(d.path("a.bin").readBytes().contentEquals(blobA))
    }

    @Test
    fun progressFractionIsClamped() {
        assertEquals(0f, DownloadState.Downloading(0, 0).fraction, 0f)
        assertEquals(0.5f, DownloadState.Downloading(50, 100).fraction, 0f)
        assertEquals(1f, DownloadState.Downloading(300, 100).fraction, 0f)
    }
}
