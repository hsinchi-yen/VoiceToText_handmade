package app.v2c.ime.asr

import app.v2c.ime.ffi.AsrHint
import app.v2c.ime.ffi.AsrPick
import app.v2c.ime.ffi.AsrPlan
import app.v2c.ime.ffi.AsrSource
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SourceMenuTest {

    private fun plan(p: AsrSource) = AsrPlan(p, AsrSource.NONE, false, AsrHint.NONE)

    @Test
    fun menuListsAutoTeaBreezeSystemLocalInOrder() {
        assertEquals(
            listOf(AsrPick.AUTO, AsrPick.TEA, AsrPick.BREEZE, AsrPick.SYSTEM, AsrPick.LOCAL),
            SourceMenuState.items,
        )
    }

    @Test
    fun startsFromTheStoredPickWithTheMenuClosed() {
        val s = SourceMenuState.load(InMemorySourcePrefs(pick = AsrPick.BREEZE))
        assertEquals(AsrPick.BREEZE, s.pick)
        assertFalse(s.menuOpen)
    }

    @Test
    fun longPressOpensAndDismissCloses() {
        val s = SourceMenuState().openMenu()
        assertTrue(s.menuOpen)
        assertFalse(s.dismiss().menuOpen)
    }

    @Test
    fun selectingStoresThePickAndClosesTheMenu() {
        val prefs = InMemorySourcePrefs()
        val (s, effect) = SourceMenuState().openMenu().select(AsrPick.TEA, prefs, localModelReady = false)
        assertEquals(AsrPick.TEA, s.pick)
        assertFalse(s.menuOpen)
        assertEquals(AsrPick.TEA, prefs.pick)
        assertEquals(SelectEffect.NONE, effect)
    }

    @Test
    fun selectionSurvivesAKeyboardRestart() {
        val prefs = InMemorySourcePrefs()
        SourceMenuState().select(AsrPick.SYSTEM, prefs, true)
        assertEquals(AsrPick.SYSTEM, SourceMenuState.load(prefs).pick)
    }

    @Test
    fun firstSwitchToUndownloadedLocalModelAsksToDownload() {
        val prefs = InMemorySourcePrefs()
        val (_, effect) = SourceMenuState().select(AsrPick.LOCAL, prefs, localModelReady = false)
        assertEquals(SelectEffect.SHOW_LOCAL_PROMPT, effect)
    }

    @Test
    fun noDownloadQuestionWhenAlreadyAskedOrAlreadyDownloaded() {
        val asked = InMemorySourcePrefs(localPromptAsked = true)
        assertEquals(SelectEffect.NONE, SourceMenuState().select(AsrPick.LOCAL, asked, false).second)
        val ready = InMemorySourcePrefs()
        assertEquals(SelectEffect.NONE, SourceMenuState().select(AsrPick.LOCAL, ready, true).second)
    }

    @Test
    fun otherPicksNeverAskToDownload() {
        val prefs = InMemorySourcePrefs()
        for (p in listOf(AsrPick.AUTO, AsrPick.TEA, AsrPick.BREEZE, AsrPick.SYSTEM)) {
            assertEquals(SelectEffect.NONE, SourceMenuState().select(p, prefs, false).second)
        }
    }

    @Test
    fun unknownStoredValueMeansAuto() {
        assertEquals(AsrPick.AUTO, SourceMenuState.parsePick(null))
        assertEquals(AsrPick.AUTO, SourceMenuState.parsePick("nonsense"))
        assertEquals(AsrPick.LOCAL, SourceMenuState.parsePick("LOCAL"))
    }

    @Test
    fun chipShowsTheForcedSourceEvenWhenUnavailable() {
        val none = AsrPlan(AsrSource.NONE, AsrSource.NONE, false, AsrHint.LAN_UNREACHABLE)
        assertEquals(SourceLabel.TEA, SourceMenuState.chipLabel(AsrPick.TEA, none))
        assertEquals(SourceLabel.LOCAL, SourceMenuState.chipLabel(AsrPick.LOCAL, null))
    }

    @Test
    fun chipInAutoFollowsThePlan() {
        assertEquals(SourceLabel.TEA, SourceMenuState.chipLabel(AsrPick.AUTO, plan(AsrSource.LAN_TEA)))
        assertEquals(SourceLabel.SYSTEM, SourceMenuState.chipLabel(AsrPick.AUTO, plan(AsrSource.SYSTEM)))
        assertEquals(SourceLabel.LOCAL, SourceMenuState.chipLabel(AsrPick.AUTO, plan(AsrSource.LOCAL)))
        assertEquals(SourceLabel.NONE, SourceMenuState.chipLabel(AsrPick.AUTO, plan(AsrSource.NONE)))
        assertEquals(SourceLabel.AUTO, SourceMenuState.chipLabel(AsrPick.AUTO, null))
    }
}
