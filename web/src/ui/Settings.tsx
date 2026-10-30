import { For, Show, createSignal } from "solid-js";
import { settingValues, setSetting } from "../data/settings";
import { discardLocalData, flushWrites, pendingWrites } from "../data/writes";
import { session, signOut } from "../session";
import { FIELDS, type Field } from "../settings/fields";
import { Confirm } from "./Confirm";

// Wireframe D5 for the web: the account, whether every change has reached
// the server, the synced playback settings and the keyboard shortcuts. The
// device-only settings (downloads, storage) have no meaning in a browser, so
// they aren't here.
export function Settings() {
  const [confirming, setConfirming] = createSignal(false);

  async function startSignOut() {
    // Give waiting changes one chance to go out, so the question below only
    // comes up when something would really be lost.
    await flushWrites().catch(() => {});
    setConfirming(true);
  }

  async function finishSignOut() {
    const userId = session()?.userId;
    setConfirming(false);
    if (userId) discardLocalData(userId);
    await signOut();
  }

  return (
    <div class="page settings">
      <header class="page-header">
        <h1>Settings</h1>
      </header>

      <section>
        <h2>Account</h2>
        <div class="group">
          <div class="setting-row">
            <div>
              <div class="setting-title">{session()?.username}</div>
              <div class="muted small">Signed in on {location.host}</div>
            </div>
            <button class="button" onClick={startSignOut}>
              Sign out
            </button>
          </div>
          <div class="setting-row">
            <div class="setting-title">Changes</div>
            <div class="sync-state" classList={{ waiting: pendingWrites() > 0 }}>
              {pendingWrites() === 0 ? "All saved to the server" : `${pendingWrites()} waiting to be sent`}
            </div>
          </div>
        </div>
      </section>

      <section>
        <h2>Playback</h2>
        <p class="muted section-note">These follow your account to every device.</p>
        <div class="group">
          <For each={FIELDS}>{(field) => <FieldRow field={field} />}</For>
        </div>
      </section>

      {/* Phones have no keyboard to speak of. */}
      <section class="wide-only">
        <h2>Keyboard</h2>
        <div class="group">
          <div class="setting-row">
            <div class="setting-title">Play or pause</div>
            <kbd>Space</kbd>
          </div>
          <div class="setting-row">
            <div class="setting-title">Search or add a podcast</div>
            <kbd>Ctrl K</kbd>
          </div>
        </div>
      </section>

      <Show when={confirming()}>
        <Confirm
          title="Sign out?"
          confirm={pendingWrites() > 0 ? "Sign out anyway" : "Sign out"}
          onConfirm={finishSignOut}
          onCancel={() => setConfirming(false)}
        >
          {pendingWrites() === 0
            ? "Your listening history is safe on the server."
            : pendingWrites() === 1
              ? "1 change hasn't reached the server yet. Signing out now discards it."
              : `${pendingWrites()} changes haven't reached the server yet. Signing out now discards them.`}
        </Confirm>
      </Show>
    </div>
  );
}

/** One synced setting as a menu of its steps. A stored value between steps is listed too, so it shows as itself. */
function FieldRow(props: { field: Field }) {
  const value = () => props.field.current(settingValues());
  const options = () => (props.field.steps.includes(value()) ? props.field.steps : [...props.field.steps, value()].sort((a, b) => a - b));
  const id = `setting-${props.field.key}`;
  return (
    <div class="setting-row">
      <label for={id}>
        <div class="setting-title">{props.field.title}</div>
        <Show when={props.field.note}>{(note) => <div class="muted small">{note()}</div>}</Show>
      </label>
      <select id={id} value={value()} onChange={(event) => setSetting(props.field.key, Number(event.currentTarget.value))}>
        <For each={options()}>{(step) => <option value={step}>{props.field.label(step)}</option>}</For>
      </select>
    </div>
  );
}
