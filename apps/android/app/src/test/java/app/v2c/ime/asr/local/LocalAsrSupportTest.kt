package app.v2c.ime.asr.local

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class LocalAsrSupportTest {
    @Test
    fun cleanStripsTagsAndTrims() {
        assertEquals("hello 你好", LocalAsrText.clean("<|zh|><|NEUTRAL|><|Speech|> hello 你好 "))
    }

    @Test
    fun pcmToFloatScales() {
        val f = LocalAsrText.pcmToFloat(shortArrayOf(0, 16384, -32768))
        assertEquals(0f, f[0], 0f)
        assertEquals(0.5f, f[1], 0f)
        assertEquals(-1f, f[2], 0f)
    }

    @Test
    fun fakeZhTwConverterIsApplied() {
        val fake = ZhTwConverter { it.replace("软件", "軟體") }
        assertEquals("軟體", fake.toZhTw("软件"))
        assertEquals("abc", ZhTwConverter.Identity.toZhTw("abc"))
    }

    @Test
    fun cerCountsEditsOnNormalizedText() {
        val s = Cer.score("今天 天氣, 很好!", "今天天器很好")
        assertEquals(6, s.referenceLength)
        assertEquals(1, s.errors)
        assertEquals(1.0 / 6, s.rate, 1e-9)
        assertEquals(0.0, Cer.score("Hello World", "hello, world").rate, 0.0)
        assertEquals(0.0, Cer.aggregate(emptyList()), 0.0)
    }

    @Test
    fun cerAggregateWeightsByLength() {
        val a = Cer.Score(1, 10)
        val b = Cer.Score(0, 30)
        assertEquals(1.0 / 40, Cer.aggregate(listOf(a, b)), 1e-9)
    }

    @Test
    fun wavRoundTrip() {
        val pcm = ShortArray(160) { (it * 100).toShort() }
        val wav = WavReader.parse(WavReader.encode(pcm, 16000))
        assertEquals(16000, wav.rate)
        assertTrue(wav.pcm.contentEquals(pcm))
        assertEquals(0.01, wav.seconds, 1e-9)
    }

    @Test(expected = IllegalArgumentException::class)
    fun wavRejectsGarbage() {
        WavReader.parse(ByteArray(64))
    }

    @Test
    fun promptAsksOnceThenHides() {
        val spec = LocalModels.DEFAULT
        val ask = LocalModelPromptPolicy.content(DownloadState.NotDownloaded, spec, alreadyAsked = false)
        assertEquals(PromptContent.Ask(spec.totalMegabytes), ask)
        assertEquals(
            PromptContent.Hidden,
            LocalModelPromptPolicy.content(DownloadState.NotDownloaded, spec, alreadyAsked = true),
        )
        assertEquals(
            PromptContent.Hidden,
            LocalModelPromptPolicy.content(DownloadState.Ready, spec, alreadyAsked = false),
        )
    }

    @Test
    fun promptShowsProgressAndFailureEvenAfterDeclining() {
        val spec = LocalModels.DEFAULT
        assertEquals(
            PromptContent.Progress(0.5f, verifying = false),
            LocalModelPromptPolicy.content(DownloadState.Downloading(5, 10), spec, alreadyAsked = true),
        )
        assertEquals(
            PromptContent.Error(DownloadFailure.HASH_MISMATCH),
            LocalModelPromptPolicy.content(
                DownloadState.Failed(DownloadFailure.HASH_MISMATCH),
                spec,
                alreadyAsked = true,
            ),
        )
    }

    @Test
    fun defaultModelSizeAndHashesLookSane() {
        val spec = LocalModels.DEFAULT
        assertEquals(237, spec.totalMegabytes)
        for (m in LocalModels.ALL) {
            for (f in m.files) assertTrue("${m.id}/${f.name}", f.sha256.matches(Regex("[0-9a-f]{64}")))
        }
    }
}
