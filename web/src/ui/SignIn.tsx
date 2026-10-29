import { createSignal, Show } from "solid-js";
import { ApiError } from "../api/http";
import { expired, lastUsername, signIn } from "../session";

// Wireframe D6. The server is the one serving this page, so unlike the apps
// there is no server field.
export function SignIn() {
  const [username, setUsername] = createSignal(lastUsername());
  const [password, setPassword] = createSignal("");
  const [register, setRegister] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await signIn(username(), password(), register());
    } catch (e) {
      // The server's messages are written for people (spec 4).
      setError(e instanceof ApiError ? e.message : "Something went wrong. Try again.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <main class="sign-in">
      <form onSubmit={submit}>
        <h1>{register() ? "Create account" : "Sign in to Pod"}</h1>
        <Show when={expired() && !register()}>
          <p class="muted">Your session ended. Sign in again to keep listening.</p>
        </Show>
        <label class="field">
          Username
          <input value={username()} onInput={(e) => setUsername(e.currentTarget.value)} autocomplete="username" required autofocus />
        </label>
        <label class="field">
          Password
          <input
            type="password"
            value={password()}
            onInput={(e) => setPassword(e.currentTarget.value)}
            autocomplete={register() ? "new-password" : "current-password"}
            minLength={register() ? 8 : undefined}
            required
          />
        </label>
        <Show when={error()}>{(message) => <p class="error" role="alert">{message()}</p>}</Show>
        <button class="button primary" type="submit" disabled={busy()}>
          {register() ? "Create account" : "Sign in"}
        </button>
        <button class="link-button" type="button" onClick={() => setRegister(!register())}>
          {register() ? "I already have an account" : "Create an account"}
        </button>
      </form>
    </main>
  );
}
