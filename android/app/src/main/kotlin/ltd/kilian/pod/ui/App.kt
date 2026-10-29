package ltd.kilian.pod.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.DownloadForOffline
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.Podcasts
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import ltd.kilian.pod.Pod
import ltd.kilian.pod.pod
import ltd.kilian.pod.downloads.observeStatuses
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.Account
import ltd.kilian.pod.sync.loadAccount
import ltd.kilian.pod.sync.loadFailures

// ==============================================================================================
// The app's shell
// ==============================================================================================
//
// Sign-in until there is an account row (wireframe M1), then four tabs with a back stack of
// detail screens on top. Every screen reads the store through live queries and writes through
// local writes, so nothing here waits on the network.

/** Where the app is. Tabs are the roots; the others stack on top of them. */
sealed interface Screen {
    enum class Tab(val label: String, val icon: ImageVector) : Screen {
        Inbox("Inbox", Icons.Outlined.Inbox),
        Podcasts("Podcasts", Icons.Outlined.Podcasts),
        Downloads("Downloads", Icons.Outlined.DownloadForOffline),
        Settings("Settings", Icons.Outlined.Settings),
    }

    data class Podcast(val id: String) : Screen
    data class Episode(val id: String) : Screen
    data object Search : Screen
}

/** What screens use to move around and to tell the user something. */
class Navigator(
    private val stack: MutableList<Screen>,
    val snackbar: SnackbarHostState,
    private val scope: CoroutineScope,
) {
    val current: Screen get() = stack.last()

    fun open(screen: Screen) {
        stack += screen
    }

    fun tab(tab: Screen.Tab) {
        stack.clear()
        stack += tab
    }

    fun back(): Boolean {
        if (stack.size <= 1) return false
        stack.removeAt(stack.lastIndex)
        return true
    }

    /** Shows [message]; [undo] runs if the user taps Undo within the snackbar's 5 s or so. */
    fun tell(message: String, undo: (suspend () -> Unit)? = null) {
        scope.launch {
            snackbar.currentSnackbarData?.dismiss()
            val result = snackbar.showSnackbar(message, actionLabel = undo?.let { "Undo" }, duration = SnackbarDuration.Short)
            if (result == SnackbarResult.ActionPerformed) undo?.invoke()
        }
    }
}

val LocalPod = staticCompositionLocalOf<Pod> { error("no Pod provided") }
val LocalNavigator = staticCompositionLocalOf<Navigator> { error("no Navigator provided") }

/** The account row as the shell sees it: still loading, absent, or there. */
private sealed interface AccountState {
    data object Loading : AccountState
    data class Loaded(val account: Account?) : AccountState
}

@Composable
fun PodRoot() {
    val pod = LocalContext.current.pod
    val account by remember { pod.database.observe("account") { AccountState.Loaded(loadAccount(it)) } }
        .collectAsState(initial = AccountState.Loading)
    CompositionLocalProvider(LocalPod provides pod) {
        when (val state = account) {
            // A blank frame for the few milliseconds the first read takes, rather than a flash of
            // the sign-in screen.
            AccountState.Loading -> Box(Modifier.fillMaxSize())
            is AccountState.Loaded -> if (state.account == null) SignInScreen() else Shell(state.account)
        }
    }
}

@Composable
private fun Shell(account: Account) {
    val pod = LocalPod.current
    val stack = remember { mutableStateListOf<Screen>(Screen.Tab.Inbox) }
    val snackbar = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    val navigator = remember { Navigator(stack, snackbar, scope) }
    var signInSheet by rememberSaveable { mutableStateOf(false) }
    var signOutDialog by rememberSaveable { mutableStateOf(false) }

    BackHandler(enabled = stack.size > 1) { navigator.back() }

    // A subscribe the server rejected (spec 5.5): the placeholder is gone, and a snackbar says
    // why. Failures already logged when the app started were shown before, so the first read
    // only marks them seen.
    LaunchedEffect(Unit) {
        var seen: Set<String>? = null
        pod.database.observe("rejected_mutation") { loadFailures(it) }.collect { failures ->
            failures.filter { seen != null && it.mutationId !in seen!! }.forEach { navigator.tell(it.message) }
            seen = failures.map { it.mutationId }.toSet()
        }
    }

    val downloads by remember { observeStatuses(pod.database, pod.reconciler) }.collectAsState(initial = emptyMap())

    CompositionLocalProvider(LocalDownloadStatuses provides downloads, LocalNavigator provides navigator, LocalSignIn provides { signInSheet = true }, LocalSignOut provides { signOutDialog = true }) {
        val current = navigator.current
        Scaffold(
            // Every screen opens with a TopAppBar, which takes the status bar inset itself.
            contentWindowInsets = ScaffoldDefaults.contentWindowInsets.only(WindowInsetsSides.Horizontal + WindowInsetsSides.Bottom),
            snackbarHost = { SnackbarHost(snackbar) },
            bottomBar = {
                // The navigation bar takes the gesture inset; without it the mini player must.
                Column(if (current is Screen.Tab) Modifier else Modifier.navigationBarsPadding()) {
                    MiniPlayer()
                    if (current is Screen.Tab) {
                        NavigationBar {
                            Screen.Tab.entries.forEach { tab ->
                                NavigationBarItem(
                                    selected = current == tab,
                                    onClick = { navigator.tab(tab) },
                                    icon = { Icon(tab.icon, contentDescription = null) },
                                    label = { Text(tab.label) },
                                )
                            }
                        }
                    }
                }
            },
        ) { padding ->
            Box(Modifier.padding(padding)) {
                when (current) {
                    Screen.Tab.Inbox -> InboxScreen()
                    Screen.Tab.Podcasts -> PodcastsScreen()
                    Screen.Tab.Downloads -> DownloadsScreen()
                    Screen.Tab.Settings -> SettingsScreen(account)
                    is Screen.Podcast -> PodcastScreen(current.id)
                    is Screen.Episode -> EpisodeScreen(current.id)
                    Screen.Search -> SearchScreen()
                }
            }
        }
        if (signInSheet) SignInAgainSheet(account, onDismiss = { signInSheet = false })
        if (signOutDialog) SignOutDialog(onDismiss = { signOutDialog = false })
    }
}

/** Opens the password sheet the "Signed out" status leads to (wireframe M1, second frame). */
val LocalSignIn = staticCompositionLocalOf<() -> Unit> { {} }

/** Opens the sign-out confirmation (wireframe M11). */
val LocalSignOut = staticCompositionLocalOf<() -> Unit> { {} }
