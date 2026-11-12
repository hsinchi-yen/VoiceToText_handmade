package app.v2c.ime.keyboard

/** What the enter key should do for the current editor. */
sealed interface EnterAction {
    data class EditorAction(val actionId: Int) : EnterAction
    data object InsertNewline : EnterAction
}

/**
 * Pure decision (constants mirror android.view.inputmethod.EditorInfo so JVM tests need no Android).
 * [imeOptions] is EditorInfo.imeOptions; null means no editor is attached.
 */
fun decideEnterAction(imeOptions: Int?): EnterAction {
    if (imeOptions == null) return EnterAction.InsertNewline
    if (imeOptions and IME_FLAG_NO_ENTER_ACTION != 0) return EnterAction.InsertNewline
    val action = imeOptions and IME_MASK_ACTION
    return if (action == IME_ACTION_NONE || action == IME_ACTION_UNSPECIFIED) {
        EnterAction.InsertNewline
    } else {
        EnterAction.EditorAction(action)
    }
}

const val IME_MASK_ACTION = 0x000000ff
const val IME_ACTION_UNSPECIFIED = 0
const val IME_ACTION_NONE = 1
const val IME_FLAG_NO_ENTER_ACTION = 0x40000000
