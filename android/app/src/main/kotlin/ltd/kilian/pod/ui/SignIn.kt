package ltd.kilian.pod.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import ltd.kilian.pod.BuildConfig
import ltd.kilian.pod.sync.Account
import ltd.kilian.pod.sync.Credentials
import ltd.kilian.pod.sync.SignInError
import ltd.kilian.pod.sync.SignInResult

// ==============================================================================================
// Sign in (wireframe M1)
// ==============================================================================================
//
// The only screen that needs the network. The server address is prefilled with the build's
// default and collapsed behind "Change server". "Create account" stays until a register attempt
// says the server doesn't take new accounts: the app learns that from the error code rather than
// a separate call.

@Composable
fun SignInScreen() {
    val pod = LocalPod.current
    val scope = rememberCoroutineScope()
    var server by rememberSaveable { mutableStateOf(BuildConfig.DEFAULT_SERVER_URL) }
    var changeServer by rememberSaveable { mutableStateOf(false) }
    var username by rememberSaveable { mutableStateOf("") }
    var password by rememberSaveable { mutableStateOf("") }
    var busy by rememberSaveable { mutableStateOf(false) }
    var error by rememberSaveable { mutableStateOf<String?>(null) }
    var registration by rememberSaveable { mutableStateOf(true) }

    fun submit(register: Boolean) {
        busy = true
        error = null
        scope.launch {
            when (val result = pod.session.signIn(Credentials(server, username, password, register))) {
                // The account row appears and the shell swaps this screen for the inbox.
                is SignInResult.Ok -> Unit
                is SignInResult.Failed -> {
                    if (result.error == SignInError.RegistrationDisabled) registration = false
                    error = result.error.message
                }
            }
            busy = false
        }
    }

    val colors = podColors
    Box(Modifier.fillMaxSize().background(colors.background).safeDrawingPadding().imePadding().verticalScroll(rememberScrollState()), contentAlignment = Alignment.Center) {
        Column(
            Modifier
                .padding(horizontal = 16.dp, vertical = 24.dp)
                .widthIn(max = 400.dp)
                .fillMaxWidth()
                .shadow(24.dp, RoundedCornerShape(20.dp), ambientColor = Color.Black.copy(alpha = 0.3f), spotColor = Color.Black.copy(alpha = 0.3f))
                .clip(RoundedCornerShape(20.dp))
                .background(colors.surface)
                .border(1.dp, colors.line, RoundedCornerShape(20.dp))
                .padding(start = 28.dp, end = 28.dp, top = 32.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Logo(Modifier.size(44.dp))
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text("Sign in to Pod", style = PodType.pageTitle.copy(fontSize = 30.sp, lineHeight = 33.sp), color = colors.ink)
                if (!changeServer) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        Text(server.substringAfter("://"), style = PodType.body.copy(fontSize = 16.sp), color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                        LinkButton("Change server", onClick = { changeServer = true })
                    }
                }
            }
            if (changeServer) {
                PodTextField(
                    server, { server = it }, label = "Server",
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            PodTextField(
                username, { username = it }, label = "Username",
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Ascii, imeAction = ImeAction.Next),
                modifier = Modifier.fillMaxWidth(),
            )
            PodTextField(
                password, { password = it }, label = "Password",
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
                modifier = Modifier.fillMaxWidth(),
            )
            error?.let { Text(it, style = PodType.body, color = colors.danger, modifier = Modifier.semantics { contentDescription = "Sign-in error" }) }
            PodButton(
                "Sign in", { submit(register = false) }, Modifier.fillMaxWidth().padding(top = 4.dp),
                kind = ButtonKind.Primary, height = 46.dp,
                enabled = !busy && username.isNotBlank() && password.isNotEmpty(),
            )
            if (registration) {
                PodButton(
                    "Create account", { submit(register = true) }, Modifier.fillMaxWidth(),
                    kind = ButtonKind.Quiet, height = 40.dp, contentColor = colors.accentInk,
                    enabled = !busy && username.isNotBlank() && password.isNotEmpty(),
                )
            }
        }
    }
}

/** The pea pod: the accent's rounded square with the pod and its three peas, as on the web (`favicon.svg`). */
@Composable
fun Logo(modifier: Modifier = Modifier) {
    val colors = podColors
    Canvas(modifier) {
        val unit = size.minDimension / 32f
        drawRoundRect(colors.accent, cornerRadius = CornerRadius(8f * unit))
        val pod = Path().apply {
            moveTo(5.5f * unit, 26.5f * unit)
            cubicTo(6f * unit, 15f * unit, 13f * unit, 6.5f * unit, 26.5f * unit, 5.5f * unit)
            cubicTo(26f * unit, 17f * unit, 18f * unit, 25.5f * unit, 5.5f * unit, 26.5f * unit)
            close()
        }
        drawPath(pod, colors.onAccent)
        drawCircle(colors.accent, 2.7f * unit, Offset(11.4f * unit, 20.6f * unit))
        drawCircle(colors.accent, 2.9f * unit, Offset(16f * unit, 16f * unit))
        drawCircle(colors.accent, 2.7f * unit, Offset(20.6f * unit, 11.4f * unit))
    }
}

/**
 * The password prompt after a `401` (spec 5.7): a sheet over the working app, never a takeover.
 * Signing in as the same user keeps local data and the outbox. "Use a different account" goes
 * through the sign-out confirmation first.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SignInAgainSheet(account: Account, onDismiss: () -> Unit) {
    val pod = LocalPod.current
    val signOut = LocalSignOut.current
    val scope = rememberCoroutineScope()
    var password by rememberSaveable { mutableStateOf("") }
    var busy by rememberSaveable { mutableStateOf(false) }
    var error by rememberSaveable { mutableStateOf<String?>(null) }
    val waiting = rememberOutboxCount()

    val colors = podColors
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = colors.background, shape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp)) {
        Column(Modifier.padding(horizontal = 24.dp).padding(bottom = 24.dp).imePadding(), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Text("Sign in again", style = PodType.detailTitle, color = colors.ink)
            Text(
                buildString {
                    append("Your session on this phone expired. Everything still works offline")
                    when (waiting) {
                        0L -> append(".")
                        1L -> append(", and 1 change will sync once you sign in.")
                        else -> append(", and $waiting changes will sync once you sign in.")
                    }
                },
                style = PodType.body, color = colors.muted,
            )
            PodTextField(
                password, { password = it }, label = "Password for ${account.username}",
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                error = error,
                modifier = Modifier.fillMaxWidth(),
            )
            PodButton(
                "Sign in",
                onClick = {
                    busy = true
                    scope.launch {
                        when (val result = pod.session.signIn(Credentials(account.serverUrl, account.username, password, register = false))) {
                            is SignInResult.Ok -> onDismiss()
                            is SignInResult.Failed -> error = result.error.message
                        }
                        busy = false
                    }
                },
                kind = ButtonKind.Primary,
                enabled = !busy && password.isNotEmpty(),
                height = 46.dp,
                modifier = Modifier.fillMaxWidth(),
            )
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                PodButton("Not now", onDismiss, kind = ButtonKind.Quiet)
                PodButton("Use a different account", {
                    onDismiss()
                    signOut()
                }, kind = ButtonKind.Quiet)
            }
        }
    }
}
