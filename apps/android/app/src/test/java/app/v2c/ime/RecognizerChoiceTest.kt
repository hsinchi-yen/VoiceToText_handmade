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
    fun noFallbackWithoutSystemRecognizerOrOnSystemOrNonLanguageError() {
        assertFalse(RecognizerChoice.shouldFallBackToSystem(RecognizerKind.ON_DEVICE, true, false))
        assertFalse(RecognizerChoice.shouldFallBackToSystem(RecognizerKind.SYSTEM, true, true))
        assertFalse(RecognizerChoice.shouldFallBackToSystem(RecognizerKind.ON_DEVICE, false, true))
    }
}
