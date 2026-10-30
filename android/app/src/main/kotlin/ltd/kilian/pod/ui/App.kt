package ltd.kilian.pod.ui

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.tween
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import ltd.kilian.pod.Pod
import ltd.kilian.pod.downloads.observeStatuses
import ltd.kilian.pod.inbox.INBOX_TABLES
import ltd.kilian.pod.inbox.loadPodcasts
import ltd.kilian.pod.pod
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
//
// The layout follows the window's width, as the web client's does:
// - Under 720 dp (phones): a tab bar, the mini player floating above it, which opens the full
//   player sheet, and an open episode as a page of its own.
// - From 720 dp (tablets, phones in landscape on the larger side): a 280 dp sidebar with the
//   tabs and the show list beside the content, and the player bar along the bottom.
// - From 760 dp of content beside the sidebar: an episode opened from a list (the inbox, a show,
//   downloads) opens beside that list instead of over it, and the list marks its row.

/** Where the app is. Tabs are the roots; the others stack on top of them. */
sealed interface Screen {
    enum class Tab(val label: String, val icon: ImageVector) : Screen {
        Inbox("Inbox", PodIcons.Inbox),
        Podcasts("Podcasts", PodIcons.Shows),
        Downloads("Downloads", PodIcons.Download),
        Settings("Settings", PodIcons.Settings),
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

    /** The screen [back] returns to, for back affordances that name it. */
    val previous: Screen? get() = stack.getOrNull(stack.lastIndex - 1)

    /**
     * True while an episode opens beside its list (wide layouts). Opening another episode then
     * replaces the open one, as picking another row in a mail client does, so Back still returns
     * to the list alone.
     */
    var detailPane = false

    fun open(screen: Screen) {
        if (detailPane && screen is Screen.Episode && current is Screen.Episode) stack.removeAt(stack.lastIndex)
        stack += screen
    }

    fun tab(tab: Screen.Tab) {
        stack.clear()
        stack += tab
    }

    /** A show from the sidebar: its page over the Podcasts tab, so Back leads to the grid. */
    fun show(id: String) {
        stack.clear()
        stack += Screen.Tab.Podcasts
        stack += Screen.Podcast(id)
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

/**
 * The app. [pod] is the process's container; screenshot tests pass one of their own, and a
 * [start] stack and an open [playerSheet] to render a given screen.
 */
@Composable
fun PodRoot(pod: Pod = LocalContext.current.pod, start: List<Screen> = listOf(Screen.Tab.Inbox), playerSheet: Boolean = false) {
    val account by remember { pod.database.observe("account") { AccountState.Loaded(loadAccount(it)) } }
        .collectAsState(initial = AccountState.Loading)
    CompositionLocalProvider(LocalPod provides pod) {
        when (val state = account) {
            // A blank frame for the few milliseconds the first read takes, rather than a flash of
            // the sign-in screen.
            AccountState.Loading -> Box(Modifier.fillMaxSize().background(podColors.background))
            is AccountState.Loaded -> if (state.account == null) SignInScreen() else Shell(state.account, start, playerSheet)
        }
    }
}

/** The width from which the sidebar and player bar replace the tab bar and mini player. */
private val WIDE = 720.dp

/** The content width from which an episode opens beside its list. */
private val SPLIT = 760.dp

@Composable
private fun Shell(account: Account, start: List<Screen>, sheetOpen: Boolean) {
    val pod = LocalPod.current
    val colors = podColors
    val stack = remember { mutableStateListOf(*start.toTypedArray()) }
    val snackbar = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    val navigator = remember { Navigator(stack, snackbar, scope) }
    var signInSheet by rememberSaveable { mutableStateOf(false) }
    var signOutDialog by rememberSaveable { mutableStateOf(false) }
    var playerSheet by rememberSaveable { mutableStateOf(sheetOpen) }

    BackHandler(enabled = stack.size > 1 && !playerSheet) { navigator.back() }

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
        BoxWithConstraints(
            Modifier
                .fillMaxSize()
                .background(colors.background)
                .windowInsetsPadding(WindowInsets.displayCutout.union(WindowInsets.navigationBars).only(WindowInsetsSides.Horizontal)),
        ) {
            val wide = maxWidth >= WIDE
            val current = navigator.current
            CompositionLocalProvider(LocalGutter provides if (wide) 28.dp else 16.dp) {
                Scaffold(
                    containerColor = colors.background,
                    contentWindowInsets = WindowInsets(0),
                    snackbarHost = { SnackbarHost(snackbar) { PodToast(it) } },
                    bottomBar = {
                        if (wide) {
                            PlayerBar()
                        } else {
                            Column {
                                MiniPlayer(onOpen = { playerSheet = true })
                                if (current is Screen.Tab) TabBar(current) else Spacer(Modifier.navigationBarsPadding())
                            }
                        }
                    },
                ) { padding ->
                    Box(Modifier.padding(padding).fillMaxSize()) {
                        if (wide) {
                            Row(Modifier.fillMaxSize()) {
                                Sidebar(current)
                                BoxWithConstraints(Modifier.weight(1f).fillMaxHeight()) {
                                    val beneath = navigator.previous
                                    val split = maxWidth >= SPLIT && current is Screen.Episode &&
                                        (beneath == Screen.Tab.Inbox || beneath == Screen.Tab.Downloads || beneath is Screen.Podcast)
                                    navigator.detailPane = split
                                    if (split) {
                                        Row(Modifier.fillMaxSize()) {
                                            Box(Modifier.weight(0.9f).fillMaxHeight()) {
                                                CompositionLocalProvider(LocalOpenEpisode provides (current as Screen.Episode).id) { Content(beneath!!, account) }
                                            }
                                            Box(Modifier.width(1.dp).fillMaxHeight().background(colors.line))
                                            Box(Modifier.weight(1.25f).fillMaxHeight().background(colors.surface).statusBarsPadding()) {
                                                EpisodeScreen(current.id, inPane = true)
                                            }
                                        }
                                    } else {
                                        Content(current, account)
                                    }
                                }
                            }
                        } else {
                            navigator.detailPane = false
                            Content(current, account)
                        }
                    }
                }
                // The sheet is a phone's; a window that widens (a rotated tablet) has the bar instead.
                LaunchedEffect(wide) { if (wide) playerSheet = false }
                AnimatedVisibility(
                    visible = playerSheet && !wide,
                    enter = slideInVertically(tween(280)) { it },
                    exit = slideOutVertically(tween(220)) { it },
                ) {
                    PlayerSheet(onClose = { playerSheet = false })
                }
            }
        }
        if (signInSheet) SignInAgainSheet(account, onDismiss = { signInSheet = false })
        if (signOutDialog) SignOutDialog(onDismiss = { signOutDialog = false })
    }
}

/** One screen. The show page draws its wash under the status bar, so it takes that inset itself. */
@Composable
private fun Content(screen: Screen, account: Account) {
    Box(if (screen is Screen.Podcast) Modifier.fillMaxSize() else Modifier.fillMaxSize().statusBarsPadding()) {
        when (screen) {
            Screen.Tab.Inbox -> InboxScreen()
            Screen.Tab.Podcasts -> PodcastsScreen()
            Screen.Tab.Downloads -> DownloadsScreen()
            Screen.Tab.Settings -> SettingsScreen(account)
            is Screen.Podcast -> PodcastScreen(screen.id)
            is Screen.Episode -> EpisodeScreen(screen.id)
            Screen.Search -> SearchScreen()
        }
    }
}

// ==============================================================================================
// Navigation: the phone's tab bar and the wide layout's sidebar
// ==============================================================================================

/** Inbox, Podcasts, Downloads, Settings: an icon above an 11 sp label; the active tab in ink with an accent icon. */
@Composable
private fun TabBar(current: Screen.Tab) {
    val colors = podColors
    val navigator = LocalNavigator.current
    Column(Modifier.fillMaxWidth().background(colors.sidebar)) {
        Box(Modifier.fillMaxWidth().height(1.dp).background(colors.line))
        Row(Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 4.dp, vertical = 4.dp)) {
            Screen.Tab.entries.forEach { tab ->
                val active = tab == current
                Column(
                    Modifier
                        .weight(1f)
                        .clip(RoundedCornerShape(12.dp))
                        .clickable(role = Role.Tab, onClickLabel = tab.label) { navigator.tab(tab) }
                        .padding(top = 6.dp, bottom = 4.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(2.dp),
                ) {
                    Icon(tab.icon, contentDescription = null, tint = if (active) colors.accent else colors.faint, modifier = Modifier.size(24.dp))
                    Text(tab.label, style = PodType.tab, color = if (active) colors.ink else colors.faint)
                }
            }
        }
    }
}

/**
 * The wide layout's navigation: the logo, a search field that opens search, the tabs, and the
 * shows under a "Podcasts" heading that opens the grid.
 */
@Composable
private fun Sidebar(current: Screen) {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val colors = podColors
    val now = rememberNow()
    val shows by remember(now / 60_000) { pod.database.observe(*INBOX_TABLES) { loadPodcasts(it, now) } }.collectAsState(initial = emptyList())
    val tabOf = when (current) {
        is Screen.Tab -> current
        is Screen.Podcast -> null
        else -> navigator.previous as? Screen.Tab
    }
    LazyColumn(
        Modifier.width(280.dp).fillMaxHeight().background(colors.sidebar).statusBarsPadding(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(start = 12.dp, end = 12.dp, top = 18.dp, bottom = 24.dp),
        verticalArrangement = Arrangement.spacedBy(1.dp),
    ) {
        item {
            Row(Modifier.padding(start = 8.dp, bottom = 18.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Logo(Modifier.size(26.dp))
                Text("Pod", style = PodType.pageTitle.copy(fontSize = 21.sp, letterSpacing = (-0.42).sp), color = colors.ink)
            }
        }
        item {
            Row(
                Modifier
                    .fillMaxWidth()
                    .padding(bottom = 14.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .background(colors.surface)
                    .clickable(role = Role.Button, onClickLabel = "Search") { navigator.open(Screen.Search) }
                    .heightIn(min = 40.dp)
                    .padding(horizontal = 11.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Icon(PodIcons.Search, contentDescription = null, tint = colors.faint, modifier = Modifier.size(18.dp))
                Text("Search or paste URL", style = PodType.body, color = colors.faint)
            }
        }
        items(listOf(Screen.Tab.Inbox, Screen.Tab.Downloads, Screen.Tab.Settings)) { tab ->
            NavItem(tab.label, active = tabOf == tab, onClick = { navigator.tab(tab) }) {
                Icon(tab.icon, contentDescription = null, tint = if (tabOf == tab) colors.accent else colors.muted, modifier = Modifier.size(20.dp))
            }
        }
        item {
            val active = tabOf == Screen.Tab.Podcasts && current !is Screen.Podcast
            Text(
                "Podcasts", style = PodType.label, color = if (active) colors.ink else colors.muted,
                modifier = Modifier
                    .padding(top = 22.dp, bottom = 6.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(if (active) colors.hover else androidx.compose.ui.graphics.Color.Transparent)
                    .clickable(role = Role.Button) { navigator.tab(Screen.Tab.Podcasts) }
                    .padding(horizontal = 10.dp, vertical = 2.dp),
            )
        }
        items(shows, key = { it.id }) { show ->
            NavItem(show.title, active = (current as? Screen.Podcast)?.id == show.id, onClick = { navigator.show(show.id) }, regular = true) {
                Artwork(show.artworkUrl, 28.dp, show.title)
            }
        }
    }
}

@Composable
private fun NavItem(label: String, active: Boolean, onClick: () -> Unit, regular: Boolean = false, leading: @Composable () -> Unit) {
    val colors = podColors
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(if (active) colors.surface else androidx.compose.ui.graphics.Color.Transparent)
            .clickable(onClick = onClick)
            .heightIn(min = if (regular) 42.dp else 40.dp)
            .padding(horizontal = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        leading()
        Text(label, style = if (regular) PodType.body else PodType.bodyMedium, color = colors.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** Opens the password sheet the "Signed out" status leads to (wireframe M1, second frame). */
val LocalSignIn = staticCompositionLocalOf<() -> Unit> { {} }

/** Opens the sign-out confirmation (wireframe M11). */
val LocalSignOut = staticCompositionLocalOf<() -> Unit> { {} }
