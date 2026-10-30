import { Show } from "solid-js";
import { Icon } from "./Icon";

/**
 * Cover art at `size` px. A show without art gets a tile with its initial, so
 * rows and grids keep their rhythm and the show stays recognisable; one whose
 * feed hasn't given a title yet gets a sound-wave glyph instead.
 */
export function Artwork(props: { url: string | null | undefined; size: number; title?: string | null }) {
  return (
    <Show
      when={props.url}
      fallback={
        // The size goes in as a custom property, not as width and height, so
        // a layout's stylesheet can still resize it like it resizes an <img>.
        <span class="artwork placeholder" style={{ "--size": `${props.size}px` }}>
          {initial(props.title) || <Icon name="waves" size={Math.round(props.size * 0.4)} />}
        </span>
      }
    >
      {(url) => <img class="artwork" src={url()} alt="" width={props.size} height={props.size} loading="lazy" />}
    </Show>
  );
}

/**
 * The cover blown up and blurred behind a surface (the player, a show's
 * header), so each show tints the screen it's on. It clips itself, so the
 * surface can still let a menu hang out of it.
 */
export function Ambient(props: { url: string | null | undefined }) {
  return (
    <Show when={props.url}>
      {(url) => (
        <span class="ambient" aria-hidden="true">
          <img src={url()} alt="" />
        </span>
      )}
    </Show>
  );
}

function initial(title: string | null | undefined): string {
  // Skip a leading "The", which would give half the shelf the same letter.
  const words = (title ?? "").replace(/^the\s+/i, "").trim();
  return [...words][0]?.toUpperCase() ?? "";
}
