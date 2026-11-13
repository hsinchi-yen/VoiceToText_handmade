package app.v2c.ime.asr

import app.v2c.ime.ErrorKind
import app.v2c.ime.FakeCloud
import app.v2c.ime.FakeFfi
import app.v2c.ime.FakeLocalAsr
import app.v2c.ime.ListLog
import app.v2c.ime.detectorAt
import app.v2c.ime.ffi.AsrHint
import app.v2c.ime.ffi.AsrPick
import app.v2c.ime.ffi.AsrPlan
import app.v2c.ime.ffi.AsrSource
import app.v2c.ime.ffi.FfiErrorCode
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.V2cFfiException
import app.v2c.ime.loadedCatalog
import app.v2c.ime.translate.TranslateFailure
import app.v2c.ime.translate.TranslateKeyState
import app.v2c.ime.translate.TranslateResult
import app.v2c.ime.translate.Translator
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** Plan section 8.1 with the cloud column: LAN -> cloud (same PCM) -> on-device. */
class CloudRoutingTest {
    private val ffi = FakeFfi()
    private val local = FakeLocalAsr(ready = true)
    private val prefs = InMemorySourcePrefs()
    private val log = ListLog()
    private val cloud = FakeCloud(asrId = "cloud-gemini", translateId = "cloud-gemini")
    private val pcm = ShortArray(16_000) { 100 }

    private fun router(lan: LanSite, c: FakeCloud = cloud) =
        RecognitionRouter(ffi, detectorAt(lan, ffi), prefs, local, log, cloud = c)

    private fun lanFails() {
        ffi.transcribe = { _, id ->
            if (id.startsWith("cloud")) "雲端" else throw V2cFfiException(FfiErrorCode.TIMEOUT)
        }
    }

    // ---- plan ----

    @Test
    fun withoutAnyCloudThePlanIsExactlyToday() {
        val none = FakeCloud()
        assertEquals(
            AsrPlan(AsrSource.SYSTEM, AsrSource.LOCAL, true, AsrHint.NONE),
            router(LanSite.UNREACHABLE, none).prepare(true).plan,
        )
        assertEquals(
            AsrPlan(AsrSource.LAN_TEA, AsrSource.LOCAL, false, AsrHint.NONE),
            router(LanSite.TN, none).prepare(true).plan,
        )
    }

    @Test
    fun lanUnreachableAndCloudReadyGoesToTheCloudBeforeTheSystemRecognizer() {
        val p = router(LanSite.UNREACHABLE).prepare(systemAvailable = true)
        assertEquals(AsrSource.CLOUD, p.plan.primary)
        assertEquals(AsrSource.LOCAL, p.plan.fallback)
    }

    @Test
    fun noValidatedNetworkMeansNoCloudAndTheSystemRecognizerIsUsed() {
        val offline = FakeCloud(asrId = "cloud-gemini", network = false)
        assertEquals(AsrSource.SYSTEM, router(LanSite.UNREACHABLE, offline).prepare(true).plan.primary)
    }

    @Test
    fun lanPlanFallsBackToCloudThenLocal() {
        val plan = router(LanSite.TN).prepare(true).plan
        assertEquals(AsrSource.LAN_TEA, plan.primary)
        assertEquals(AsrSource.CLOUD, plan.fallback)
        assertEquals(AsrSource.LOCAL, plan.fallback2)
    }

    // ---- fallback order ----

    @Test
    fun lanFailureUsesTheCloudWithTheSamePcm() {
        lanFails()
        val r = router(LanSite.TN)
        val res = r.recognizePcm(r.prepare(true), pcm) as RecognizeResult.Text
        assertEquals("tw:雲端", res.text)
        assertEquals(AsrSource.CLOUD, res.source)
        assertTrue(res.fellBack)
        assertEquals(listOf("tn-tea-asr", "cloud-gemini"), ffi.transcribeCalls.map { it.second })
        assertEquals(ffi.transcribeCalls[0].first, ffi.transcribeCalls[1].first) // same recording
        assertTrue(local.calls.isEmpty())
    }

    @Test
    fun lanAndCloudFailureFallBackToTheLocalModelWithTheSamePcm() {
        ffi.transcribe = { _, _ -> throw V2cFfiException(FfiErrorCode.TIMEOUT) }
        local.result = { "本機" }
        val r = router(LanSite.TN)
        val res = r.recognizePcm(r.prepare(true), pcm) as RecognizeResult.Text
        assertEquals("本機", res.text)
        assertEquals(AsrSource.LOCAL, res.source)
        assertSame(pcm, local.calls.single())
        assertEquals(listOf("tn-tea-asr", "cloud-gemini"), ffi.transcribeCalls.map { it.second })
    }

    @Test
    fun lanAndCloudFailureWithoutLocalAsksToSpeakAgain() {
        ffi.transcribe = { _, _ -> throw V2cFfiException(FfiErrorCode.TIMEOUT) }
        local.ready = false
        val r = router(LanSite.TN)
        assertEquals(
            RecognizeResult.Failure(RouteFailure.LAN_FAILED_SAY_AGAIN),
            r.recognizePcm(r.prepare(true), pcm),
        )
    }

    @Test
    fun cloudPrimaryFailureFallsBackToLocalOrAsksToSpeakAgain() {
        ffi.transcribe = { _, _ -> throw V2cFfiException(FfiErrorCode.UNAUTHORIZED) }
        local.result = { "本機" }
        val r = router(LanSite.UNREACHABLE)
        val res = r.recognizePcm(r.prepare(true), pcm) as RecognizeResult.Text
        assertEquals(AsrSource.LOCAL, res.source)
        assertTrue(res.fellBack)

        local.ready = false
        assertEquals(
            RecognizeResult.Failure(RouteFailure.CLOUD_FAILED_SAY_AGAIN),
            r.recognizePcm(r.prepare(true), pcm),
        )
    }

    @Test
    fun cloudPrimarySuccessConvertsToZhTw() {
        ffi.transcribe = { _, _ -> "你好" }
        val r = router(LanSite.UNREACHABLE)
        val res = r.recognizePcm(r.prepare(true), pcm) as RecognizeResult.Text
        assertEquals("tw:你好", res.text)
        assertEquals(AsrSource.CLOUD, res.source)
        assertFalse(res.fellBack)
        assertEquals("cloud-gemini", ffi.transcribeCalls.single().second)
    }

    // ---- manual pick ----

    @Test
    fun forcedCloudNeverFallsBackAndReportsWhyWhenNotReady() {
        prefs.pick = AsrPick.CLOUD
        val ready = router(LanSite.TN).prepare(true)
        assertEquals(AsrSource.CLOUD, ready.plan.primary)
        assertEquals(AsrSource.NONE, ready.plan.fallback)

        val offline = router(LanSite.TN, FakeCloud(asrId = "cloud-gemini", network = false)).prepare(true)
        assertEquals(AsrSource.NONE, offline.plan.primary)
        assertEquals(ErrorKind.CLOUD_NOT_READY, ErrorMapping.forHint(offline.plan.hint))
    }

    @Test
    fun aForcedCloudPickAfterTheServiceWasRemovedGoesBackToAuto() {
        prefs.pick = AsrPick.CLOUD
        val p = router(LanSite.TN, FakeCloud()).prepare(true)
        assertEquals(AsrSource.LAN_TEA, p.plan.primary)
    }

    @Test
    fun logsHoldOnlyNamesLengthsAndCodes() {
        ffi.transcribe = { _, _ -> "機密內容" }
        val r = router(LanSite.UNREACHABLE)
        r.recognizePcm(r.prepare(true), pcm)
        assertTrue(log.lines.any { it.contains("cloud") && it.contains("chars=") })
        assertTrue(log.lines.none { it.contains("機密") })
    }

    // ---- menu and chip ----

    @Test
    fun theCloudEntryAppearsOnlyWhenAServiceIsConfigured() {
        assertEquals(SourceMenuState.items, SourceMenuState.itemsFor(false))
        assertFalse(AsrPick.CLOUD in SourceMenuState.itemsFor(false))
        val with = SourceMenuState.itemsFor(true)
        assertEquals(
            listOf(AsrPick.AUTO, AsrPick.TEA, AsrPick.BREEZE, AsrPick.CLOUD, AsrPick.SYSTEM, AsrPick.LOCAL),
            with,
        )
    }

    @Test
    fun theChipShowsTheServiceNameWhenCloudIsInUse() {
        val plan = AsrPlan(AsrSource.CLOUD, AsrSource.NONE, true, AsrHint.NONE)
        assertEquals("Gemini", SourceMenuState.chipText(AsrPick.AUTO, plan, "Gemini"))
        assertEquals("雲端", SourceMenuState.chipText(AsrPick.AUTO, plan, null))
        assertEquals("Gemini", SourceMenuState.chipText(AsrPick.CLOUD, null, "Gemini"))
        // Other sources keep their own names.
        assertEquals("TEA", SourceMenuState.chipText(AsrPick.TEA, plan, "Gemini"))
        assertEquals("系統", SourceMenuState.chipText(AsrPick.AUTO, AsrPlan(AsrSource.SYSTEM, AsrSource.NONE, true, AsrHint.NONE), "Gemini"))
    }

    @Test
    fun parsingTheStoredCloudPickWorks() {
        assertEquals(AsrPick.CLOUD, SourceMenuState.parsePick("CLOUD"))
    }

    // ---- translation order ----

    private fun translator(lan: LanSite, c: FakeCloud = cloud) =
        Translator(ffi, detectorAt(lan, ffi), loadedCatalog(ffi), log, cloud = c)

    @Test
    fun translationPrefersTheLanQwenThenTheCloud() {
        assertEquals(TranslateResult.Text("en:你好"), translator(LanSite.TN).translate("你好"))
        assertEquals("tn-qwen", ffi.translateCalls.last().second)

        assertEquals(TranslateResult.Text("en:你好"), translator(LanSite.UNREACHABLE).translate("你好"))
        assertEquals("cloud-gemini", ffi.translateCalls.last().second)
    }

    @Test
    fun translationWithoutLanAndWithoutAReadyCloudNeedsOneOfThem() {
        val before = ffi.translateCalls.size
        val none = FakeCloud()
        assertEquals(
            TranslateResult.Failure(TranslateFailure.NEEDS_LAN),
            translator(LanSite.UNREACHABLE, none).translate("你好"),
        )
        val offline = FakeCloud(translateId = "cloud-gemini", network = false)
        assertEquals(
            TranslateResult.Failure(TranslateFailure.NEEDS_LAN),
            translator(LanSite.UNREACHABLE, offline).translate("你好"),
        )
        assertEquals(before, ffi.translateCalls.size)
    }

    @Test
    fun theTranslateKeyIsEnabledByALanOrAReadyCloudService() {
        assertTrue(translator(LanSite.UNREACHABLE).keyState().enabled)
        assertFalse(translator(LanSite.UNREACHABLE, FakeCloud()).keyState().enabled)
        assertTrue(translator(LanSite.TN, FakeCloud()).keyState().enabled)
        assertTrue(TranslateKeyState.of(LanSite.UNREACHABLE, cloudTranslateReady = true).enabled)
        assertFalse(TranslateKeyState.of(LanSite.UNREACHABLE).enabled)
    }

    @Test
    fun aCloudTranslationFailureIsAFailureAndLogsNoText() {
        ffi.translateImpl = { _, _ -> throw V2cFfiException(FfiErrorCode.UNAUTHORIZED) }
        assertEquals(
            TranslateResult.Failure(TranslateFailure.FAILED),
            translator(LanSite.UNREACHABLE).translate("機密"),
        )
        assertTrue(log.lines.any { it.contains("cloud") && it.contains("code=") })
        assertTrue(log.lines.none { it.contains("機密") })
    }
}
