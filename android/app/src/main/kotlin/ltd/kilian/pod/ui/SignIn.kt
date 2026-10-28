package ltd.kilian.pod.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
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

    Surface(Modifier.fillMaxSize()) {
        Column(
            Modifier.safeDrawingPadding().imePadding().verticalScroll(rememberScrollState()).padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Spacer(Modifier.height(48.dp))
            Text("Pod", style = MaterialTheme.typography.displayMedium, color = MaterialTheme.colorScheme.primary)
            Text("Your podcasts, on every device", style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(24.dp))
            OutlinedTextField(
                username, { username = it }, label = { Text("Username") }, singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Ascii, imeAction = ImeAction.Next),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                password, { password = it }, label = { Text("Password") }, singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
                modifier = Modifier.fillMaxWidth(),
            )
            if (changeServer) {
                OutlinedTextField(
                    server, { server = it }, label = { Text("Server") }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                    modifier = Modifier.fillMaxWidth(),
                )
            } else {
                Row {
                    Text(server.substringAfter("://"), Modifier.padding(vertical = 12.dp), style = MaterialTheme.typography.bodyMedium)
                    TextButton(onClick = { changeServer = true }) { Text("Change server") }
                }
            }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.semantics { contentDescription = "Sign-in error" }) }
            Button(onClick = { submit(register = false) }, enabled = !busy && username.isNotBlank() && password.isNotEmpty(), modifier = Modifier.fillMaxWidth()) {
                Text("Sign in")
            }
            if (registration) {
                OutlinedButton(onClick = { submit(register = true) }, enabled = !busy && username.isNotBlank() && password.isNotEmpty(), modifier = Modifier.fillMaxWidth()) {
                    Text("Create account")
                }
            }
        }
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

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 24.dp).padding(bottom = 24.dp).imePadding(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Sign in again", style = MaterialTheme.typography.titleLarge)
            Text(
                buildString {
                    append("Your session on this phone expired. Everything still works offline")
                    when (waiting) {
                        0L -> append(".")
                        1L -> append(", and 1 change will sync once you sign in.")
                        else -> append(", and $waiting changes will sync once you sign in.")
                    }
                },
            )
            OutlinedTextField(
                password, { password = it }, label = { Text("Password for ${account.username}") }, singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                modifier = Modifier.fillMaxWidth(),
            )
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Button(
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
                enabled = !busy && password.isNotEmpty(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Sign in") }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                TextButton(onClick = onDismiss) { Text("Not now") }
                TextButton(onClick = {
                    onDismiss()
                    signOut()
                }) { Text("Use a different account") }
            }
        }
    }
}
