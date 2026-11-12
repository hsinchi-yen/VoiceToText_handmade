package app.v2c.ime

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RecognizerChoiceTest {
    @Test
    fun prefersOnDeviceWhenAvailableAndSupported() {
        assertEquals(RecognizerKind.ON_DEVICE, RecognizerChoice.choose(true, true, false))
    }

    @Test
    fun skipsOnDeviceOnceMarkedUnsupported() {
        assertEquals(RecognizerKind.SYSTEM, RecognizerChoice.choose(true, true, true))
    }

    @Test
    fun usesSystemWhenNoOnDeviceService() {
        assertEquals(RecognizerKind.SYSTEM, RecognizerChoice.choose(false, true, false))
    }

    @Test
    fun noneWhenNothingAvailable() {
        assertEquals(RecognizerKind.NONE, RecognizerChoice.choose(false, false, false))
        assertEquals(RecognizerKind.NONE, RecognizerChoice.choose(true, false, true))
    }

    @Test
    fun languageErrorOnOnDeviceFallsBack() {
        assertTrue(RecognizerChoice.shouldFallBackToSystem(RecognizerKind.ON_DEVICE, true, true))
    }

    @Test
    fun onDeviceLanguageErrorFallsBackAndAsksToSayAgain() {
        val o = RecognizerChoice.resolveError(RecognizerKind.ON_DEVICE, ErrorClass.LANGUAGE, true)
        assertEquals(ErrorKind.SAY_AGAIN, o.kind)
        assertTrue(o.markOnDeviceUnsupported)
    }

    @Test
    fun systemLanguageErrorIsNotTheInstallHint() {
        val o = RecognizerChoice.resolveError(RecognizerKind.SYSTEM, ErrorClass.LANGUAGE, true)
        assertEquals(ErrorKind.LANGUAGE_UNSUPPORTED, o.kind)
        assertFalse(o.markOnDeviceUnsupported)
    }

    @Test
    fun onDeviceLanguageErrorWithoutSystemRecognizerIsNotTheInstallHint() {
        val o = RecognizerChoice.resolveError(RecognizerKind.ON_DEVICE, ErrorClass.LANGUAGE, false)
        assertEquals(ErrorKind.LANGUAGE_UNSUPPORTED, o.kind)
        assertFalse(o.markOnDeviceUnsupported)
    }

    @Test
    fun otherErrorClassesMapToTheirKinds() {
        val k = RecognizerKind.SYSTEM
        assertEquals(ErrorKind.NO_SPEECH, RecognizerChoice.resolveError(k, ErrorClass.NO_SPEECH, true).kind)
        assertEquals(ErrorKind.NETWORK, RecognizerChoice.resolveError(k, ErrorClass.NETWORK, true).kind)
        assertEquals(ErrorKind.NO_PERMISSION, RecognizerChoice.resolveError(k, ErrorClass.PERMISSION, true).kind)
        assertEquals(ErrorKind.GENERIC, RecognizerChoice.resolveError(k, ErrorClass.OTHER, true).kind)
    }

    @Test
    fun noFallbackWithoutSystemRecognizerOrOnSystemOrNonLanguageError() {
        assertFalse(RecognizerChoice.shouldFallBackToSystem(RecognizerKind.ON_DEVICE, true, false))
        assertFalse(RecognizerChoice.shouldFallBackToSystem(RecognizerKind.SYSTEM, true, true))
        assertFalse(RecognizerChoice.shouldFallBackToSystem(RecognizerKind.ON_DEVICE, false, true))
    }
}
