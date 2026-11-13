package app.v2c.ime.cloud

import app.v2c.ime.FakeCipher
import app.v2c.ime.FakeFfi
import app.v2c.ime.R
import app.v2c.ime.ffi.CloudTestResult
import app.v2c.ime.ffi.CloudUse
import app.v2c.ime.ffi.FfiErrorCode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CloudSetupLogicTest {
    private val gemini = CloudServiceSettings.suggested(CloudKind.GEMINI)

    @Test
    fun suggestedModelsAndKeyPagesMatchTheDesktop() {
        assertEquals("gemini-3.6-flash", CloudKind.GEMINI.suggestedAsrModel)
        assertEquals("gpt-transcribe", CloudKind.OPENAI.suggestedAsrModel)
        assertEquals("claude-haiku-5-5", CloudKind.CLAUDE.suggestedModel)
        assertEquals("https://aistudio.google.com/apikey", CloudKind.GEMINI.keyPageUrl)
        assertEquals("https://platform.openai.com/api-keys", CloudKind.OPENAI.keyPageUrl)
        assertEquals("https://platform.claude.com/settings/keys", CloudKind.CLAUDE.keyPageUrl)
        assertTrue(CloudKind.entries.mapNotNull { it.keyPageUrl }.all { it.startsWith("https://") })
    }

    @Test
    fun serviceIdsAreUniqueAndNeverALanId() {
        val ids = CloudKind.entries.map { it.serviceId }
        assertEquals(ids.size, ids.toSet().size)
        assertTrue(ids.none { it.startsWith("tn-") || it.startsWith("rd-") })
    }

    @Test
    fun draftValidation() {
        assertNull(CloudSetupLogic.validate(gemini, keyAvailable = true))
        assertEquals(DraftProblem.KEY_MISSING, CloudSetupLogic.validate(gemini, keyAvailable = false))
        assertEquals(DraftProblem.NO_USE, CloudSetupLogic.validate(gemini.copy(uses = emptySet()), true))
        assertEquals(DraftProblem.MODEL_MISSING, CloudSetupLogic.validate(gemini.copy(model = " "), true))
        // Claude with only ASR selected has no valid use left.
        val claude = CloudServiceSettings.suggested(CloudKind.CLAUDE).copy(uses = setOf(CloudUse.ASR))
        assertEquals(DraftProblem.NO_USE, CloudSetupLogic.validate(claude, true))
    }

    @Test
    fun customEndpointNeedsAnHttpsUrlAndNoKey() {
        val custom = CloudServiceSettings.suggested(CloudKind.CUSTOM).copy(model = "m", asrModel = "w")
        assertEquals(DraftProblem.BASE_URL_INVALID, CloudSetupLogic.validate(custom, false))
        assertEquals(
            DraftProblem.BASE_URL_INVALID,
            CloudSetupLogic.validate(custom.copy(baseUrl = "http://10.0.0.5/v1"), false),
        )
        assertNull(CloudSetupLogic.validate(custom.copy(baseUrl = "https://example.com/v1"), keyAvailable = false))
        assertNull(CloudSetupLogic.validate(custom.copy(baseUrl = "http://localhost:8080/v1"), keyAvailable = false))
        assertFalse(CloudSetupLogic.validCustomUrl("not a url"))
    }

    @Test
    fun errorCodesMapToReadableReasons() {
        assertEquals(R.string.cloud_test_unauthorized, CloudSetupLogic.testErrorMessage(FfiErrorCode.UNAUTHORIZED))
        assertEquals(R.string.cloud_test_rate_limited, CloudSetupLogic.testErrorMessage(FfiErrorCode.RATE_LIMITED))
        assertEquals(R.string.cloud_test_unreachable, CloudSetupLogic.testErrorMessage(FfiErrorCode.UNREACHABLE))
        assertEquals(R.string.cloud_test_unsupported, CloudSetupLogic.testErrorMessage(FfiErrorCode.UNSUPPORTED))
        assertEquals(R.string.cloud_test_other, CloudSetupLogic.testErrorMessage(FfiErrorCode.INTERNAL))
    }

    @Test
    fun testingADraftUsesTheTypedKeyAndTestsEachUse() {
        val ffi = FakeFfi()
        ffi.testResult = { _, use ->
            if (use == CloudUse.ASR) CloudTestResult(0, FfiErrorCode.UNAUTHORIZED) else CloudTestResult(250, null)
        }
        val results = CloudSetupLogic.testDraft(ffi, gemini, " typed-key ", CloudSnapshot.EMPTY)
        assertEquals(listOf(CloudUse.ASR, CloudUse.TRANSLATE), results.map { it.use })
        assertEquals(FfiErrorCode.UNAUTHORIZED, results[0].error)
        assertEquals(250L, results[1].latencyMs)
        assertEquals("typed-key", ffi.cloudSets.single().second["cloud-gemini"])
    }

    @Test
    fun testingFallsBackToTheStoredKeyAndKeepsOtherServices() {
        val repo = CloudRepository(InMemoryKeyValueStore(), CloudKeyVault(InMemoryKeyValueStore(), FakeCipher()))
        repo.save(CloudServiceSettings.suggested(CloudKind.OPENAI).copy(uses = setOf(CloudUse.TRANSLATE)), "openai-key")
        repo.save(gemini.copy(uses = setOf(CloudUse.ASR)), "stored-gemini")
        val ffi = FakeFfi()
        CloudSetupLogic.testDraft(ffi, gemini.copy(uses = setOf(CloudUse.ASR)), "", repo.snapshot())
        val (services, keys) = ffi.cloudSets.single()
        assertEquals(setOf("cloud-openai", "cloud-gemini"), services.map { it.id }.toSet())
        assertEquals("stored-gemini", keys["cloud-gemini"])
        assertEquals("openai-key", keys["cloud-openai"])
    }

    @Test
    fun aRejectedConfigIsReportedPerUse() {
        val ffi = FakeFfi().apply { setCloudFails = true }
        val results = CloudSetupLogic.testDraft(ffi, gemini, "k", CloudSnapshot.EMPTY)
        assertTrue(results.all { it.error == FfiErrorCode.CONFIG })
    }
}
