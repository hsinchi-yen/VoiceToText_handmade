package app.v2c.ime.asr

import app.v2c.ime.ErrorClass
import app.v2c.ime.ErrorKind
import app.v2c.ime.FakeFfi
import app.v2c.ime.FakeLocalAsr
import app.v2c.ime.ListLog
import app.v2c.ime.detectorAt
import app.v2c.ime.ffi.AsrHint
import app.v2c.ime.ffi.AsrPick
import app.v2c.ime.ffi.AsrSource
import app.v2c.ime.ffi.FfiErrorCode
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.V2cFfiException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class RecognitionRouterTest {
    private val ffi = FakeFfi()
    private val local = FakeLocalAsr(ready = true)
    private val prefs = InMemorySourcePrefs()
    private val log = ListLog()

    private fun router(lan: LanSite) = RecognitionRouter(ffi, detectorAt(lan, ffi), prefs, local, log)

    private val pcm = ShortArray(16_000) { 100 } // 1 s

    private fun lanFails() {
        ffi.transcribe = { _, _ -> throw V2cFfiException(FfiErrorCode.TIMEOUT) }
    }

    // ---- LAN primary ----

    @Test
    fun onTheCompanyLanAutoUsesTeaAndConvertsToZhTw() {
        val r = router(LanSite.TN)
        val p = r.prepare(systemAvailable = true)
        assertEquals(AsrSource.LAN_TEA, p.plan.primary)
        val res = r.recognizePcm(p, pcm) as RecognizeResult.Text
        assertEquals("tw:raw", res.text)
        assertEquals(AsrSource.LAN_TEA, res.source)
        assertFalse(res.fellBack)
        assertEquals(listOf(pcm.size * 2 to "tn-tea-asr"), ffi.transcribeCalls.toList())
    }

    @Test
    fun rdLanUsesRdServiceId() {
        val r = router(LanSite.RD)
        r.recognizePcm(r.prepare(true), pcm)
        assertEquals("rd-tea-asr", ffi.transcribeCalls.single().second)
    }

    @Test
    fun forcedBreezeUsesBreezeService() {
        prefs.pick = AsrPick.BREEZE
        val r = router(LanSite.TN)
        r.recognizePcm(r.prepare(true), pcm)
        assertEquals("tn-breeze-asr", ffi.transcribeCalls.single().second)
    }

    @Test
    fun lanFailureWithLocalModelReusesTheSamePcm() {
        lanFails()
        local.result = { "本機結果" }
        val r = router(LanSite.TN)
        val res = r.recognizePcm(r.prepare(true), pcm) as RecognizeResult.Text
        assertEquals("本機結果", res.text)
        assertEquals(AsrSource.LOCAL, res.source)
        assertTrue(res.fellBack)
        assertEquals(1, local.calls.size)
        assertSame(pcm, local.calls.single()) // same recording, no second take
    }

    @Test
    fun lanFailureWithoutLocalModelAsksToSpeakAgain() {
        lanFails()
        local.ready = false
        val r = router(LanSite.TN)
        val res = r.recognizePcm(r.prepare(true), pcm)
        assertEquals(RecognizeResult.Failure(RouteFailure.LAN_FAILED_SAY_AGAIN), res)
        assertTrue(local.calls.isEmpty())
    }

    @Test
    fun aForcedLanSourceNeverFallsBack() {
        lanFails()
        prefs.pick = AsrPick.TEA
        val r = router(LanSite.TN)
        val res = r.recognizePcm(r.prepare(true), pcm)
        assertEquals(RecognizeResult.Failure(RouteFailure.LAN_FAILED_SAY_AGAIN), res)
        assertTrue(local.calls.isEmpty())
    }

    @Test
    fun lanFallbackToLocalThatAlsoFailsIsAGenericFailure() {
        lanFails()
        local.result = { throw IllegalStateException("model") }
        val r = router(LanSite.TN)
        assertEquals(RecognizeResult.Failure(RouteFailure.FAILED), r.recognizePcm(r.prepare(true), pcm))
    }

    // ---- no LAN ----

    @Test
    fun withoutLanAutoUsesTheSystemRecognizer() {
        val r = router(LanSite.UNREACHABLE)
        assertEquals(AsrSource.SYSTEM, r.prepare(true).plan.primary)
    }

    @Test
    fun withoutLanAndWithoutSystemAutoUsesLocalWhenReady() {
        val r = router(LanSite.UNREACHABLE)
        val p = r.prepare(systemAvailable = false)
        assertEquals(AsrSource.LOCAL, p.plan.primary)
        val res = r.recognizePcm(p, pcm) as RecognizeResult.Text
        assertEquals("local", res.text)
        assertFalse(res.fellBack)
    }

    @Test
    fun nothingAvailableGivesAHintInsteadOfSilentlySwitching() {
        local.ready = false
        val p = router(LanSite.UNREACHABLE).prepare(systemAvailable = false)
        assertEquals(AsrSource.NONE, p.plan.primary)
        assertEquals(AsrHint.NO_SOURCE_AVAILABLE, p.plan.hint)
    }

    @Test
    fun forcedUnusableSourcesReportTheirReason() {
        local.ready = false
        prefs.pick = AsrPick.LOCAL
        val p = router(LanSite.TN).prepare(true)
        assertEquals(AsrSource.NONE, p.plan.primary)
        assertEquals(ErrorKind.LOCAL_NOT_READY, ErrorMapping.forHint(p.plan.hint))
        prefs.pick = AsrPick.TEA
        assertEquals(AsrHint.LAN_UNREACHABLE, router(LanSite.UNREACHABLE).prepare(true).plan.hint)
    }

    // ---- system recognizer failure ----

    @Test
    fun systemFailureWithLocalModelMakesTheNextUtteranceLocalOnce() {
        val r = router(LanSite.UNREACHABLE)
        val p = r.prepare(true)
        assertTrue(r.systemFailed(p, ErrorClass.OTHER))
        assertEquals(AsrSource.LOCAL, r.preview(true).primary)
        assertEquals(AsrSource.LOCAL, r.prepare(true).plan.primary) // user speaks again
        assertEquals(AsrSource.SYSTEM, r.prepare(true).plan.primary) // one-shot only
    }

    @Test
    fun systemFailureWithoutLocalModelDoesNotSwitch() {
        local.ready = false
        val r = router(LanSite.UNREACHABLE)
        assertFalse(r.systemFailed(r.prepare(true), ErrorClass.NETWORK))
        assertEquals(AsrSource.SYSTEM, r.prepare(true).plan.primary)
    }

    @Test
    fun noSpeechAndPermissionErrorsDoNotSwitchToLocal() {
        val r = router(LanSite.UNREACHABLE)
        val p = r.prepare(true)
        assertFalse(r.systemFailed(p, ErrorClass.NO_SPEECH))
        assertFalse(r.systemFailed(p, ErrorClass.PERMISSION))
    }

    @Test
    fun aForcedSystemPickNeverFallsBack() {
        prefs.pick = AsrPick.SYSTEM
        val r = router(LanSite.UNREACHABLE)
        assertFalse(r.systemFailed(r.prepare(true), ErrorClass.OTHER))
    }

    @Test
    fun changingThePickClearsThePendingLocalSwitch() {
        val r = router(LanSite.UNREACHABLE)
        r.systemFailed(r.prepare(true), ErrorClass.OTHER)
        r.pickChanged()
        assertEquals(AsrSource.SYSTEM, r.prepare(true).plan.primary)
    }

    @Test
    fun systemTextIsConvertedToZhTwAndKeptIfConversionFails() {
        val r = router(LanSite.UNREACHABLE)
        assertEquals("tw:你好", (r.finishSystemText("你好") as RecognizeResult.Text).text)
        ffi.zhTwFails = true
        assertEquals("你好", (r.finishSystemText("你好") as RecognizeResult.Text).text)
        assertEquals(RecognizeResult.Failure(RouteFailure.NO_SPEECH), r.finishSystemText("  "))
    }

    // ---- audio handling ----

    @Test
    fun audioShorterThanAThirdOfASecondIsNoSpeech() {
        val r = router(LanSite.TN)
        val res = r.recognizePcm(r.prepare(true), ShortArray(100))
        assertEquals(RecognizeResult.Failure(RouteFailure.NO_SPEECH), res)
        assertTrue(ffi.transcribeCalls.isEmpty())
    }

    @Test
    fun blankTranscriptIsNoSpeech() {
        ffi.transcribe = { _, _ -> "   " }
        val r = router(LanSite.TN)
        assertEquals(RecognizeResult.Failure(RouteFailure.NO_SPEECH), r.recognizePcm(r.prepare(true), pcm))
    }

    @Test
    fun longRecordingsAreSentInPiecesOfAtMost30Seconds() {
        var n = 0
        ffi.transcribe = { _, _ -> "p${n++}" }
        val long = ShortArray(16_000 * 70) { 100 }
        val r = router(LanSite.TN)
        val res = r.recognizePcm(r.prepare(true), long) as RecognizeResult.Text
        assertTrue(ffi.transcribeCalls.size >= 3)
        assertTrue(ffi.transcribeCalls.all { it.first <= 16_000 * 30 * 2 })
        assertEquals("tw:" + (0 until n).joinToString("") { "p$it" }, res.text)
    }

    @Test
    fun longRecordingsAreAlsoChunkedForTheLocalModel() {
        val r = router(LanSite.UNREACHABLE)
        prefs.pick = AsrPick.LOCAL
        r.recognizePcm(r.prepare(false), ShortArray(16_000 * 65) { 100 })
        assertTrue(local.calls.size >= 3)
        assertTrue(local.calls.all { it.size <= 16_000 * 30 })
    }

    // ---- logging ----

    @Test
    fun logsNeverContainTranscribedText() {
        val secret = "機密內容XYZ"
        ffi.transcribe = { _, _ -> secret }
        local.result = { secret }
        val r = router(LanSite.TN)
        r.recognizePcm(r.prepare(true), pcm)
        lanFails()
        r.recognizePcm(r.prepare(true), pcm) // falls back to local
        r.finishSystemText(secret)
        assertTrue(log.lines.isNotEmpty())
        assertTrue(log.lines.none { it.contains("機密") || it.contains("XYZ") })
        assertTrue(log.lines.any { it.contains("chars=") })
    }
}

