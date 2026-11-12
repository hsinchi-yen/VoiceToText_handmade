package app.v2c.ime

import app.v2c.ime.TapHoldDetector.Action
import org.junit.Assert.assertEquals
import org.junit.Test

class TapHoldDetectorTest {
    private val d = TapHoldDetector(holdThresholdMs = 300)

    @Test
    fun pressStartsRecording() {
        assertEquals(Action.START, d.onPress(0))
    }

    @Test
    fun quickTapKeepsRecording() {
        d.onPress(0)
        assertEquals(Action.NONE, d.onRelease(120))
    }

    @Test
    fun secondTapStopsOnPress() {
        d.onPress(0)
        d.onRelease(100)
        assertEquals(Action.STOP, d.onPress(2_000))
    }

    @Test
    fun releaseAfterStoppingTapIsSwallowed() {
        d.onPress(0)
        d.onRelease(100)
        d.onPress(2_000)
        assertEquals(Action.NONE, d.onRelease(2_050))
    }

    @Test
    fun longPressStopsOnRelease() {
        d.onPress(0)
        assertEquals(Action.STOP, d.onRelease(300))
    }

    @Test
    fun justUnderThresholdIsATap() {
        d.onPress(0)
        assertEquals(Action.NONE, d.onRelease(299))
    }

    @Test
    fun sessionCanRestartAfterHold() {
        d.onPress(0)
        d.onRelease(1_000)
        assertEquals(Action.START, d.onPress(1_500))
    }

    @Test
    fun releaseWithoutPressDoesNothing() {
        assertEquals(Action.NONE, d.onRelease(50))
    }

    @Test
    fun resetAfterExternalEndAllowsNewStart() {
        d.onPress(0)
        d.onRelease(100)
        d.reset()
        assertEquals(Action.START, d.onPress(5_000))
    }

    @Test
    fun resetClearsPendingSwallow() {
        d.onPress(0)
        d.onRelease(100)
        d.onPress(2_000)
        d.reset()
        d.onPress(3_000)
        assertEquals(Action.NONE, d.onRelease(3_100))
        assertEquals(Action.STOP, d.onPress(4_000))
    }

    @Test
    fun defaultThresholdIs300() {
        assertEquals(300L, TapHoldDetector.HOLD_THRESHOLD_MS)
    }
}
