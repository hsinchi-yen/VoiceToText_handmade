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
    fun quickSecondTapWithin300msStops() {
        d.onPress(0)
        d.onRelease(50)
        assertEquals(Action.STOP, d.onPress(150))
        assertEquals(Action.NONE, d.onRelease(200))
    }

    @Test
    fun zeroDurationPressIsATap() {
        assertEquals(Action.START, d.onPress(100))
        assertEquals(Action.NONE, d.onRelease(100))
        assertEquals(Action.STOP, d.onPress(900))
    }

    @Test
    fun secondPressHeldLongStillSwallowsItsRelease() {
        d.onPress(0)
        d.onRelease(100)
        assertEquals(Action.STOP, d.onPress(2_000))
        // Held well past the threshold, but the session is already over: no second STOP.
        assertEquals(Action.NONE, d.onRelease(5_000))
        assertEquals(Action.START, d.onPress(6_000))
    }

    /**
     * Cancel handling: the UI treats a cancelled gesture as a release (see KeyboardView), so a
     * cancel is just onRelease. The detector needs no separate cancel path; owners call reset()
     * if the session dies for any other reason.
     */
    @Test
    fun cancelledGestureBehavesLikeRelease() {
        d.onPress(0)
        assertEquals(Action.STOP, d.onRelease(1_000))
    }

    @Test
    fun defaultThresholdIs300() {
        assertEquals(300L, TapHoldDetector.HOLD_THRESHOLD_MS)
    }
}
