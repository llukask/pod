import { For, createSignal } from "solid-js";

// Short notices along the bottom, such as a subscribe the server refused
// (spec 5.5). Each one leaves after a few seconds or when dismissed.

type Toast = { id: number; message: string };

const [toasts, setToasts] = createSignal<Toast[]>([]);
let nextId = 0;

const SHOWN_MS = 6_000;

export function notify(message: string) {
  const id = nextId++;
  setToasts((list) => [...list, { id, message }]);
  setTimeout(() => dismiss(id), SHOWN_MS);
}

function dismiss(id: number) {
  setToasts((list) => list.filter((toast) => toast.id !== id));
}

export function Toasts() {
  return (
    <div class="toasts" role="status" aria-live="polite">
      <For each={toasts()}>
        {(toast) => (
          <div class="toast">
            <span>{toast.message}</span>
            <button class="icon-button" title="Dismiss" onClick={() => dismiss(toast.id)}>
              ✕
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
