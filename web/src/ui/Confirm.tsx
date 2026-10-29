import { type JSX, onMount } from "solid-js";

/** A modal question with a cancel and a confirm button (M11, D7). Escape and the backdrop cancel. */
export function Confirm(props: { title: string; children: JSX.Element; confirm: string; onConfirm: () => void; onCancel: () => void }) {
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  return (
    <dialog
      ref={dialog}
      class="confirm"
      onClose={() => props.onCancel()}
      onClick={(event) => event.target === dialog && dialog.close()}
    >
      <h2>{props.title}</h2>
      <div class="muted">{props.children}</div>
      <div class="dialog-buttons">
        <button class="button" onClick={() => dialog.close()}>
          Cancel
        </button>
        <button class="button primary" autofocus onClick={() => props.onConfirm()}>
          {props.confirm}
        </button>
      </div>
    </dialog>
  );
}
