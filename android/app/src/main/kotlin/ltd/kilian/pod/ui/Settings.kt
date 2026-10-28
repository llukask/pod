package ltd.kilian.pod.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import ltd.kilian.pod.settings.loadRejected
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.Account
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
// couldn't be saved; background sync runs hourly through WorkManager and has no setting.
//
// TODO: the synced playback settings and this device's download settings, once the player's
// and the reconciler's readers exist to share their defaults.

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(account: Account) {
    val pod = LocalPod.current
    val scope = rememberCoroutineScope()
    val signOut = LocalSignOut.current
    val state by pod.engine.state.collectAsState()
    val now = rememberNow()
    val waiting = rememberOutboxCount()
    val rejected by remember { pod.database.observe("rejected_mutation", "episode") { loadRejected(it) } }.collectAsState(initial = emptyList())
    var showRejected by rememberSaveable { mutableStateOf(false) }

    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        TopAppBar(title = { Text("Settings") })
        Section("Account")
        ListItem(
            headlineContent = { Text(account.username) },
            supportingContent = { Text("${account.serverUrl.substringAfter("://")} · ${android.os.Build.MODEL}") },
            trailingContent = { OutlinedButton(onClick = signOut) { Text("Sign out") } },
        )
        Section("Sync")
        ListItem(
            headlineContent = { Text(statusLabel(state, waiting, monotonicMs(), now)) },
            supportingContent = countsLabel(waiting, rejected.size.toLong())?.let { { Text(it) } },
            trailingContent = { TextButton(onClick = { scope.launch { pod.engine.sync() } }) { Text("Sync now") } },
            modifier = Modifier.clickable(enabled = rejected.isNotEmpty()) { showRejected = !showRejected },
        )
        if (showRejected) {
            rejected.forEach { ListItem(headlineContent = { Text(it.change) }, supportingContent = { Text(it.reason) }) }
        }
    }
}

@Composable
private fun Section(title: String) {
    Text(title, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary, modifier = Modifier.padding(start = 16.dp, top = 24.dp, bottom = 4.dp))
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
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Sign out?") },
        text = { Text(signOutMessage(loaded)) },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
        confirmButton = {
            TextButton(onClick = {
                onDismiss()
                scope.launch { pod.session.logout() }
            }) { Text(if (loaded.unsent > 0) "Sign out anyway" else "Sign out") }
        },
    )
}

/** What signing out discards, in words. */
fun signOutMessage(summary: LogoutSummary): String {
    val downloads = if (summary.downloadBytes > 0) ", along with ${bytesLabel(summary.downloadBytes)} of downloads" else ""
    return when (summary.unsent) {
        0L -> "Your listening history is safe on the server. This phone's podcasts and downloads are removed$downloads."
        1L -> "1 change hasn't reached the server yet. Signing out now discards it$downloads."
        else -> "${summary.unsent} changes haven't reached the server yet. Signing out now discards them$downloads."
    }
}

/** "48 MB", "1.4 GB": decimal units, as Android's storage settings show them. */
fun bytesLabel(bytes: Long): String = when {
    bytes >= 1_000_000_000 -> "%.1f GB".format(bytes / 1e9)
    bytes >= 1_000_000 -> "${bytes / 1_000_000} MB"
    else -> "${maxOf(1, bytes / 1_000)} kB"
}
