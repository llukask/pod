import { Show } from "solid-js";

/** Cover art at `size` px, or a plain tile while a show has none. */
export function Artwork(props: { url: string | null | undefined; size: number }) {
  return (
    <Show when={props.url} fallback={<span class="artwork" style={{ width: `${props.size}px`, height: `${props.size}px` }} />}>
      {(url) => <img class="artwork" src={url()} alt="" width={props.size} height={props.size} loading="lazy" />}
    </Show>
  );
}
