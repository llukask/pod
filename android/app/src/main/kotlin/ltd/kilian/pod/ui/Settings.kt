package ltd.kilian.pod.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import ltd.kilian.pod.downloads.formatBytes
import ltd.kilian.pod.downloads.DOWNLOAD_ON_METERED
import ltd.kilian.pod.settings.Current
import ltd.kilian.pod.settings.DEVICE_FIELDS
import ltd.kilian.pod.settings.Field
import ltd.kilian.pod.settings.SETTINGS_TABLES
import ltd.kilian.pod.settings.SYNCED_FIELDS
import ltd.kilian.pod.settings.loadCurrent
import ltd.kilian.pod.settings.loadRejected
import ltd.kilian.pod.settings.save
import kotlinx.serialization.json.JsonPrimitive
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.Account
import ltd.kilian.pod.sync.EngineState
import ltd.kilian.pod.sync.LogoutSummary
import ltd.kilian.pod.sync.countsLabel
import ltd.kilian.pod.sync.loadLogoutSummary
import ltd.kilian.pod.sync.monotonicMs
import ltd.kilian.pod.sync.statusLabel

// ==============================================================================================
// Settings (wireframe M10)
// ==============================================================================================
//
// Two kinds of setting, labelled: "Synced" follows the account to every device (spec 2.4), "This
// device" stays here (7.1). The sync row shows the last sync, the outbox and the changes that
// couldn't be saved; background sync runs hourly through WorkManager and has no setting. The
// pickers come from `settings/Fields.kt`.

@Composable
fun SettingsScreen(account: Account) {
    val pod = LocalPod.current
    val scope = rememberCoroutineScope()
    val signOut = LocalSignOut.current
    val colors = podColors
    val state by pod.engine.state.collectAsState()
    val now = rememberNow()
    val waiting = rememberOutboxCount()
    val rejected by remember { pod.database.observe("rejected_mutation", "episode") { loadRejected(it) } }.collectAsState(initial = emptyList())
    var showRejected by rememberSaveable { mutableStateOf(false) }
    val settings by remember { pod.database.observe(*SETTINGS_TABLES) { loadCurrent(it) } }.collectAsState(initial = null)
    var field by remember { mutableStateOf<Field?>(null) }

    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        PageHeader("Settings")
        Column(Modifier.padding(horizontal = LocalGutter.current).padding(bottom = 32.dp).widthIn(max = 640.dp)) {
            SectionTitle("Account")
            Panel {
                PanelRow(account.username, note = "Signed in on ${account.serverUrl.substringAfter("://")} · ${android.os.Build.MODEL}", first = true) {
                    PodButton("Sign out", signOut)
                }
                PanelRow(
                    "Changes",
                    note = countsLabel(waiting, rejected.size.toLong()),
                    onClick = if (rejected.isNotEmpty()) ({ showRejected = !showRejected }) else null,
                ) {
                    StatusDot(statusLabel(state, waiting, monotonicMs(), now), waiting = waiting > 0 || state !is EngineState.Idle, Modifier.weight(1f, fill = false))
                    LinkButton("Sync now", onClick = { scope.launch { pod.engine.sync() } })
                }
                if (showRejected) {
                    rejected.forEach { PanelRow(it.change, note = it.reason) }
                }
            }
            val current = settings ?: return@Column
            SectionTitle("Playback", "These follow your account to every device.")
            Panel {
                SYNCED_FIELDS.forEachIndexed { index, it -> FieldRow(it, current, first = index == 0) { field = it } }
            }
            SectionTitle("Downloads", "These stay on this phone.")
            Panel {
                FieldRow(DEVICE_FIELDS.first(), current, first = true) { field = it }
                PanelRow("Download on mobile data", note = if (current.downloads.onMetered) "All downloads, including ones you start" else "Downloads wait for Wi-Fi") {
                    Switch(
                        current.downloads.onMetered,
                        onCheckedChange = { on -> scope.launch { save(pod.database, pod.sync, DOWNLOAD_ON_METERED, JsonPrimitive(on)) } },
                        colors = podSwitchColors(),
                    )
                }
                DEVICE_FIELDS.drop(1).forEach { FieldRow(it, current) { field = it } }
            }
        }
    }
    val picking = field
    val current = settings
    if (picking != null && current != null) {
        StepPicker(picking, picking.current(current), onDismiss = { field = null }) { value ->
            field = null
            scope.launch { save(pod.database, pod.sync, picking.key, value) }
        }
    }
}

/** A setting and its value; tapping it calls [onPick]. */
@Composable
private fun FieldRow(field: Field, current: Current, first: Boolean = false, onPick: (Field) -> Unit) {
    val colors = podColors
    PanelRow(field.title, note = field.note, first = first, onClick = { onPick(field) }) {
        Text(field.describe(current), style = PodType.body.tabular(), color = colors.ink)
        Icon(PodIcons.ChevronRight, contentDescription = null, tint = colors.faint, modifier = Modifier.size(18.dp))
    }
}

/** The steps as a list; the current one carries a check. A stored value between steps marks none. */
@Composable
private fun StepPicker(field: Field, selected: JsonPrimitive, onDismiss: () -> Unit, onPick: (JsonPrimitive) -> Unit) {
    val colors = podColors
    AlertDialog(
        onDismissRequest = onDismiss,
        shape = RoundedCornerShape(18.dp),
        containerColor = colors.surface,
        tonalElevation = 0.dp,
        title = { Text(field.title, style = PodType.detailTitle, color = colors.ink) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                field.steps.forEach { step ->
                    val chosen = step == selected
                    Row(
                        Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).background(if (chosen) colors.accentSoft else androidx.compose.ui.graphics.Color.Transparent)
                            .clickable { onPick(step) }.padding(horizontal = 12.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(field.label(step), style = PodType.body.tabular(), color = if (chosen) colors.accentInk else colors.ink, modifier = Modifier.weight(1f))
                        if (chosen) Icon(PodIcons.Check, contentDescription = "Selected", tint = colors.accentInk, modifier = Modifier.size(18.dp))
                    }
                }
            }
        },
        confirmButton = { PodButton("Cancel", onDismiss) },
    )
}

/**
 * The sign-out confirmation (wireframe M11). Sign out wipes local data except device settings
 * (spec 5.7), so it names the unsent changes and the downloads that go with it.
 */
@Composable
fun SignOutDialog(onDismiss: () -> Unit) {
    val pod = LocalPod.current
    val scope = rememberCoroutineScope()
    var summary by remember { mutableStateOf<LogoutSummary?>(null) }
    LaunchedEffect(Unit) { summary = withContext(Dispatchers.IO) { loadLogoutSummary(pod.database.openHelper.readableDatabase) } }
    val loaded = summary ?: return
    PodDialog(
        title = "Sign out?",
        message = signOutMessage(loaded),
        confirm = if (loaded.unsent > 0) "Sign out anyway" else "Sign out",
        onConfirm = {
            onDismiss()
            scope.launch { pod.session.logout() }
        },
        onDismiss = onDismiss,
    )
}

/** What signing out discards, in words. */
fun signOutMessage(summary: LogoutSummary): String {
    val downloads = if (summary.downloadBytes > 0) ", along with ${formatBytes(summary.downloadBytes)} of downloads" else ""
    return when (summary.unsent) {
        0L -> "Your listening history is safe on the server. This phone's podcasts and downloads are removed$downloads."
        1L -> "1 change hasn't reached the server yet. Signing out now discards it$downloads."
        else -> "${summary.unsent} changes haven't reached the server yet. Signing out now discards them$downloads."
    }
}
