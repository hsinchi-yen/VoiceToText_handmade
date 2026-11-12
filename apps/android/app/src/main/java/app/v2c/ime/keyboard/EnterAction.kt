package app.v2c.ime.keyboard

import android.view.inputmethod.EditorInfo

/** What the enter key should do for the current editor. */
sealed interface EnterAction {
    data class EditorAction(val actionId: Int) : EnterAction
    data object InsertNewline : EnterAction
}

/**
 * Pure decision. [imeOptions] is EditorInfo.imeOptions; null means no editor is attached.
 * The EditorInfo constants are compile-time inlined, so this runs in plain JVM tests.
 */
fun decideEnterAction(imeOptions: Int?): EnterAction {
    if (imeOptions == null) return EnterAction.InsertNewline
    if (imeOptions and EditorInfo.IME_FLAG_NO_ENTER_ACTION != 0) return EnterAction.InsertNewline
    val action = imeOptions and EditorInfo.IME_MASK_ACTION
    return if (action == EditorInfo.IME_ACTION_NONE || action == EditorInfo.IME_ACTION_UNSPECIFIED) {
        EnterAction.InsertNewline
    } else {
        EnterAction.EditorAction(action)
    }
}
