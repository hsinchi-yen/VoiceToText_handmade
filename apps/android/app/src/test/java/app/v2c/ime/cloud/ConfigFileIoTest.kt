package app.v2c.ime.cloud

import app.v2c.ime.FakeCipher
import app.v2c.ime.FakeFfi
import app.v2c.ime.R
import app.v2c.ime.ffi.CloudServiceConfig
import app.v2c.ime.ffi.CloudUse
import app.v2c.ime.ffi.ConfigFilePreview
import app.v2c.ime.ffi.ImportStatus
import app.v2c.ime.ffi.ImportedService
import java.io.ByteArrayInputStream
import java.util.Calendar
import java.util.GregorianCalendar
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ConfigFileIoTest {
    private val settings = InMemoryKeyValueStore()
    private val keys = InMemoryKeyValueStore()
    private val repo = CloudRepository(settings, CloudKeyVault(keys, FakeCipher()))

    private fun imported(
        kind: CloudKind,
        status: ImportStatus,
        uses: List<CloudUse> = emptyList(),
        baseUrl: String? = null,
        needsKey: Boolean = true,
        dropOldKey: Boolean = false,
        model: String = kind.suggestedModel,
    ) = ImportedService(
        sourceId = "file-${kind.name.lowercase()}",
        config = CloudServiceConfig(
            id = kind.serviceId,
            provider = kind.provider,
            model = model,
            asrModel = kind.suggestedAsrModel.ifEmpty { null },
            baseUrl = baseUrl,
            uses = uses,
        ),
        status = status,
        needsKey = needsKey,
        dropOldKey = dropOldKey,
    )

    private fun preview(vararg s: ImportedService) =
        ConfigFilePreview(null, null, null, null, s.toList(), emptyList(), null, null, emptyList())

    private fun saved(kind: CloudKind, vararg uses: CloudUse, key: String? = null) {
        repo.save(CloudServiceSettings.suggested(kind).copy(uses = uses.toSet()), key)
    }

    @Test
    fun newServicesAreAddedWithTheirUsesAndTypedKeys() {
        val p = preview(
            imported(CloudKind.GEMINI, ImportStatus.ADDED, listOf(CloudUse.ASR)),
            imported(CloudKind.CLAUDE, ImportStatus.ADDED, listOf(CloudUse.TRANSLATE)),
        )
        val r = ConfigFileIo.apply(repo, p, mapOf("cloud-gemini" to " AIzaTYPED1234 "))
        assertEquals(ApplyResult(2, emptyList()), r)
        assertEquals(setOf(CloudUse.ASR), repo.service(CloudKind.GEMINI)!!.uses)
        assertEquals(setOf(CloudUse.TRANSLATE), repo.service(CloudKind.CLAUDE)!!.uses)
        assertEquals("1234", repo.lastFour(CloudKind.GEMINI))
        assertFalse("no key was typed for Claude", repo.hasKey(CloudKind.CLAUDE))
    }

    @Test
    fun aUseTakenByTheFileMovesAwayFromTheOldService() {
        saved(CloudKind.OPENAI, CloudUse.ASR, CloudUse.TRANSLATE, key = "OLD-OPENAI-0001")
        ConfigFileIo.apply(repo, preview(imported(CloudKind.GEMINI, ImportStatus.ADDED, listOf(CloudUse.ASR))), emptyMap())
        assertEquals(setOf(CloudUse.TRANSLATE), repo.service(CloudKind.OPENAI)!!.uses)
        assertEquals(setOf(CloudUse.ASR), repo.service(CloudKind.GEMINI)!!.uses)
        assertEquals("0001", repo.lastFour(CloudKind.OPENAI))
    }

    @Test
    fun existingUsesStayWhenTheFileSaysNothingAboutThem() {
        saved(CloudKind.GEMINI, CloudUse.ASR, CloudUse.TRANSLATE, key = "KEY-GEM-7777")
        val p = preview(imported(CloudKind.GEMINI, ImportStatus.UPDATED, emptyList(), model = "gemini-other", needsKey = false))
        ConfigFileIo.apply(repo, p, emptyMap())
        val s = repo.service(CloudKind.GEMINI)!!
        assertEquals("gemini-other", s.model)
        assertEquals(setOf(CloudUse.ASR, CloudUse.TRANSLATE), s.uses)
        assertEquals("7777", repo.lastFour(CloudKind.GEMINI))
    }

    @Test
    fun aChangedEndpointDropsTheOldKeyEvenWhenNoNewKeyIsTyped() {
        saved(CloudKind.CUSTOM, CloudUse.TRANSLATE, key = "OLD-KEY-5555")
        val p = preview(
            imported(
                CloudKind.CUSTOM, ImportStatus.UPDATED, listOf(CloudUse.TRANSLATE),
                baseUrl = "https://new.example.com/v1", dropOldKey = true, model = "m",
            ),
        )
        ConfigFileIo.apply(repo, p, emptyMap())
        assertFalse(repo.hasKey(CloudKind.CUSTOM))
        assertEquals("https://new.example.com/v1", repo.service(CloudKind.CUSTOM)!!.baseUrl)
        assertTrue("old key must not survive in storage", keys.map.isEmpty())
    }

    @Test
    fun aChangedEndpointWithATypedKeyStoresTheNewKey() {
        saved(CloudKind.CUSTOM, CloudUse.TRANSLATE, key = "OLD-KEY-5555")
        val p = preview(
            imported(
                CloudKind.CUSTOM, ImportStatus.UPDATED, listOf(CloudUse.TRANSLATE),
                baseUrl = "https://new.example.com/v1", dropOldKey = true, model = "m",
            ),
        )
        ConfigFileIo.apply(repo, p, mapOf("cloud-custom" to "NEW-KEY-9999"))
        assertEquals("9999", repo.lastFour(CloudKind.CUSTOM))
    }

    @Test
    fun servicesNotInTheFileAndTheirKeysAreUntouched() {
        saved(CloudKind.CLAUDE, CloudUse.TRANSLATE, key = "CLAUDE-KEY-4242")
        ConfigFileIo.apply(repo, preview(imported(CloudKind.GEMINI, ImportStatus.ADDED, listOf(CloudUse.ASR))), emptyMap())
        assertEquals(setOf(CloudUse.TRANSLATE), repo.service(CloudKind.CLAUDE)!!.uses)
        assertEquals("4242", repo.lastFour(CloudKind.CLAUDE))
    }

    @Test
    fun anUnchangedServiceWithNothingNewIsNotRewritten() {
        saved(CloudKind.GEMINI, CloudUse.ASR, key = "KEY-GEM-7777")
        val p = preview(imported(CloudKind.GEMINI, ImportStatus.UNCHANGED, listOf(CloudUse.ASR), needsKey = false))
        assertFalse(ConfigFileIo.hasChanges(repo, p))
        assertEquals(ApplyResult(0, emptyList()), ConfigFileIo.apply(repo, p, emptyMap()))
        // A new use is a change.
        val q = preview(imported(CloudKind.GEMINI, ImportStatus.UNCHANGED, listOf(CloudUse.TRANSLATE), needsKey = false))
        assertTrue(ConfigFileIo.hasChanges(repo, q))
    }

    @Test
    fun aKeystoreFailureIsReportedPerService() {
        val broken = CloudRepository(
            InMemoryKeyValueStore(),
            CloudKeyVault(InMemoryKeyValueStore(), FakeCipher(throwing = true)),
        )
        val p = preview(imported(CloudKind.GEMINI, ImportStatus.ADDED, listOf(CloudUse.ASR)))
        val r = ConfigFileIo.apply(broken, p, mapOf("cloud-gemini" to "KEY"))
        assertEquals(ApplyResult(0, listOf(CloudKind.GEMINI)), r)
        assertTrue(broken.services().isEmpty())
    }

    @Test
    fun previewHandsTheCurrentServicesAndOnlyKeyIdsToRust() {
        saved(CloudKind.GEMINI, CloudUse.ASR, key = "AIzaSECRETKEY1234")
        val ffi = FakeFfi()
        ConfigFileIo.preview(ffi, repo, "toml text")
        val (text, current, storedKeys) = ffi.parseCalls.single()
        assertEquals("toml text", text)
        assertEquals(listOf("cloud-gemini"), current.map { it.id })
        assertEquals(listOf("cloud-gemini"), storedKeys)
        assertFalse(current.toString().contains("SECRETKEY"))
    }

    @Test
    fun exportMapsUsesToIdsAndNeverTouchesKeyMaterial() {
        saved(CloudKind.GEMINI, CloudUse.ASR, key = "AIzaSECRETKEY1234")
        saved(CloudKind.CLAUDE, CloudUse.TRANSLATE, key = "CLAUDE-SECRETKEY-77")
        val ffi = FakeFfi()
        val out = ConfigFileIo.export(ffi, repo)
        val call = ffi.exportCalls.single()
        assertEquals("cloud-gemini", call[1])
        assertEquals("cloud-claude", call[2])
        assertEquals(listOf("cloud-gemini", "cloud-claude"), call[3])
        assertFalse(call.toString().contains("SECRETKEY"))
        assertFalse(out.text.contains("SECRETKEY"))
    }

    @Test
    fun exportWithoutAnyUseSendsNullIds() {
        saved(CloudKind.GEMINI)
        val ffi = FakeFfi()
        ConfigFileIo.export(ffi, repo)
        val call = ffi.exportCalls.single()
        assertNull(call[1])
        assertNull(call[2])
        assertEquals(emptyList<String>(), call[3])
    }

    @Test
    fun filesOverTheLimitAreRefusedWithoutReadingThemAll() {
        val ok = ByteArray(ConfigFileIo.MAX_BYTES) { 'a'.code.toByte() }
        assertEquals(ConfigFileIo.MAX_BYTES, ConfigFileIo.readLimited(ByteArrayInputStream(ok))!!.length)
        assertNull(ConfigFileIo.readLimited(ByteArrayInputStream(ByteArray(ConfigFileIo.MAX_BYTES + 1))))
        assertEquals("你好", ConfigFileIo.readLimited(ByteArrayInputStream("你好".toByteArray())))
    }

    @Test
    fun defaultFileNameUsesTheDate() {
        assertEquals("v2c-config-20261113.toml", ConfigFileIo.defaultFileName(GregorianCalendar(2026, Calendar.NOVEMBER, 13)))
        assertEquals("v2c-config-20260105.toml", ConfigFileIo.defaultFileName(GregorianCalendar(2026, Calendar.JANUARY, 5)))
    }

    @Test
    fun errorsAndIgnoredPartsHaveFriendlyMessages() {
        assertEquals(R.string.cfg_err_too_new, ConfigFileIo.errorMessage("format_too_new"))
        assertEquals(R.string.cfg_err_other, ConfigFileIo.errorMessage("anything_else"))
        assertEquals(R.string.cfg_ignored_vocab, ConfigFileIo.ignoredLabel("vocabulary"))
        assertNull(ConfigFileIo.ignoredLabel("hotkey"))
    }

    @Test
    fun aDroppedKeyGetsAKeyFieldEvenWhenTheFileDoesNotListIt() {
        val p = preview(
            imported(CloudKind.CUSTOM, ImportStatus.UPDATED, needsKey = false, dropOldKey = true, baseUrl = "https://x.example.com/v1"),
            imported(CloudKind.GEMINI, ImportStatus.ADDED, needsKey = false),
        )
        assertEquals(listOf("cloud-custom"), ConfigFileIo.needingKeys(p).map { it.config.id })
    }
}
