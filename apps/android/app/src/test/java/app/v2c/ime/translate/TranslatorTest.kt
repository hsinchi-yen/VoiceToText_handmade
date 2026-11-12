package app.v2c.ime.translate

import app.v2c.ime.ErrorKind
import app.v2c.ime.FakeFfi
import app.v2c.ime.ListLog
import app.v2c.ime.asr.ErrorMapping
import app.v2c.ime.asr.Finished
import app.v2c.ime.asr.RecognizeResult
import app.v2c.ime.asr.RouteFailure
import app.v2c.ime.asr.VoiceMode
import app.v2c.ime.detectorAt
import app.v2c.ime.ffi.AsrSource
import app.v2c.ime.ffi.FfiErrorCode
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.V2cFfiException
import app.v2c.ime.loadedCatalog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TranslatorTest {
    private val ffi = FakeFfi()
    private val log = ListLog()

    private fun translator(lan: LanSite) = Translator(ffi, detectorAt(lan, ffi), loadedCatalog(ffi), log)

    @Test
    fun keyIsEnabledOnlyOnACompanyLan() {
        assertTrue(translator(LanSite.TN).keyState().enabled)
        assertTrue(translator(LanSite.RD).keyState().enabled)
        assertFalse(translator(LanSite.UNREACHABLE).keyState().enabled)
        assertFalse(TranslateKeyState.of(LanSite.UNREACHABLE).enabled)
    }

    @Test
    fun usesTheCurrentLansQwen() {
        assertEquals(TranslateResult.Text("en:你好"), translator(LanSite.TN).translate("你好"))
        assertEquals("tn-qwen", ffi.translateCalls.last().second)
        translator(LanSite.RD).translate("你好")
        assertEquals("rd-qwen", ffi.translateCalls.last().second)
    }

    @Test
    fun withoutLanItNeedsTheCompanyNetworkAndDoesNotCallOut() {
        val res = translator(LanSite.UNREACHABLE).translate("你好")
        assertEquals(TranslateResult.Failure(TranslateFailure.NEEDS_LAN), res)
        assertTrue(ffi.translateCalls.isEmpty())
    }

    @Test
    fun serverErrorsAndBlankInputAreFailures() {
        ffi.translateImpl = { _, _ -> throw V2cFfiException(FfiErrorCode.TIMEOUT) }
        assertEquals(TranslateResult.Failure(TranslateFailure.FAILED), translator(LanSite.TN).translate("你好"))
        assertEquals(TranslateResult.Failure(TranslateFailure.EMPTY), translator(LanSite.TN).translate("  "))
        ffi.translateImpl = { _, _ -> " " }
        assertEquals(TranslateResult.Failure(TranslateFailure.EMPTY), translator(LanSite.TN).translate("你好"))
    }

    @Test
    fun logsNeverContainText() {
        ffi.translateImpl = { _, _ -> "SECRET-ENGLISH" }
        translator(LanSite.TN).translate("機密中文")
        assertTrue(log.lines.isNotEmpty())
        assertTrue(log.lines.none { it.contains("機密") || it.contains("SECRET") })
    }

    // ---- finishing an utterance (mic key vs translate key) ----

    private val text = RecognizeResult.Text("你好", AsrSource.LAN_TEA, false)

    @Test
    fun dictateCommitsTheChineseTextUntouched() {
        assertEquals(Finished.Commit("你好"), ErrorMapping.finish(text, VoiceMode.DICTATE, translator(LanSite.TN)))
        assertTrue(ffi.translateCalls.isEmpty())
    }

    @Test
    fun translateKeyCommitsOnlyTheEnglish() {
        assertEquals(Finished.Commit("en:你好"), ErrorMapping.finish(text, VoiceMode.TRANSLATE, translator(LanSite.TN)))
    }

    @Test
    fun failedTranslationNeverCommitsTheChinese() {
        ffi.translateImpl = { _, _ -> throw V2cFfiException(FfiErrorCode.UNREACHABLE) }
        assertEquals(
            Finished.Error(ErrorKind.TRANSLATE_FAILED),
            ErrorMapping.finish(text, VoiceMode.TRANSLATE, translator(LanSite.TN)),
        )
        assertEquals(
            Finished.Error(ErrorKind.NEEDS_LAN),
            ErrorMapping.finish(text, VoiceMode.TRANSLATE, translator(LanSite.UNREACHABLE)),
        )
    }

    @Test
    fun recognitionFailuresMapToKeyboardErrors() {
        val t = translator(LanSite.TN)
        fun f(r: RouteFailure) = ErrorMapping.finish(RecognizeResult.Failure(r), VoiceMode.DICTATE, t)
        assertEquals(Finished.Error(ErrorKind.NO_SPEECH), f(RouteFailure.NO_SPEECH))
        assertEquals(Finished.Error(ErrorKind.LAN_FAILED), f(RouteFailure.LAN_FAILED_SAY_AGAIN))
        assertEquals(Finished.Error(ErrorKind.GENERIC), f(RouteFailure.FAILED))
    }

    @Test
    fun hintsMapToReasons() {
        assertEquals(null, ErrorMapping.forHint(app.v2c.ime.ffi.AsrHint.NONE))
        assertEquals(ErrorKind.NO_SOURCE, ErrorMapping.forHint(app.v2c.ime.ffi.AsrHint.NO_SOURCE_AVAILABLE))
        assertEquals(ErrorKind.LAN_UNREACHABLE, ErrorMapping.forHint(app.v2c.ime.ffi.AsrHint.LAN_UNREACHABLE))
        assertEquals(ErrorKind.UNAVAILABLE, ErrorMapping.forHint(app.v2c.ime.ffi.AsrHint.SYSTEM_UNAVAILABLE))
        assertEquals(ErrorKind.LOCAL_NOT_READY, ErrorMapping.forHint(app.v2c.ime.ffi.AsrHint.LOCAL_MODEL_NOT_READY))
    }
}
