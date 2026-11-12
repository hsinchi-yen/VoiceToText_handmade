package app.v2c.ime.keyboard

import android.view.inputmethod.EditorInfo
import org.junit.Assert.assertEquals
import org.junit.Test

class EnterActionTest {
    @Test fun noEditorInsertsNewline() = assertEquals(EnterAction.InsertNewline, decideEnterAction(null))

    @Test fun unspecifiedInsertsNewline() = assertEquals(EnterAction.InsertNewline, decideEnterAction(0))

    @Test fun noneInsertsNewline() = assertEquals(EnterAction.InsertNewline, decideEnterAction(EditorInfo.IME_ACTION_NONE))

    @Test fun sendPerformsAction() = assertEquals(EnterAction.EditorAction(4), decideEnterAction(4))

    @Test fun searchWithOtherFlagsPerformsAction() =
        assertEquals(EnterAction.EditorAction(3), decideEnterAction(3 or 0x10000000))

    @Test fun noEnterActionFlagWins() =
        assertEquals(EnterAction.InsertNewline, decideEnterAction(4 or EditorInfo.IME_FLAG_NO_ENTER_ACTION))

    @Test fun noneWithOtherFlagsInsertsNewline() =
        assertEquals(EnterAction.InsertNewline, decideEnterAction(EditorInfo.IME_ACTION_NONE or 0x10000000))
}
