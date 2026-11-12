package app.v2c.ime.asr.local

/** Why a download did not finish. Only codes; no text from the user ever appears here. */
enum class DownloadFailure {
    /** Connection dropped or the server failed. Partial data is kept; retry resumes. */
    INTERRUPTED,

    /** The finished file did not match the pinned SHA-256. The file is discarded and never used. */
    HASH_MISMATCH,

    /** Unexpected size or HTTP status that retrying the same URL will not fix. */
    BAD_RESPONSE,

    /** Could not write to app storage (disk full, permissions). */
    STORAGE,
}

/** State of one local model download. Pure data so it can be unit tested and shown in Compose. */
sealed interface DownloadState {
    /** Nothing on disk (or user deleted it). */
    data object NotDownloaded : DownloadState

    data class Downloading(val bytesDone: Long, val bytesTotal: Long) : DownloadState {
        val fraction: Float
            get() = if (bytesTotal <= 0) 0f else (bytesDone.toFloat() / bytesTotal).coerceIn(0f, 1f)
    }

    /** All bytes received; hashing. */
    data object Verifying : DownloadState

    /** Every file verified and in place. */
    data object Ready : DownloadState

    data class Failed(val reason: DownloadFailure) : DownloadState
}
