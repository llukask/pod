import type { JSX } from "solid-js";

// The interface's icons, drawn on a 24 px grid with round 1.75 px strokes so
// they sit beside Radio Canada's text weight. Filled shapes (play, pause) are
// filled so they read at a glance inside small buttons. Every icon is
// decorative: the button around it carries the name. Each entry is a function
// because Solid's JSX makes DOM nodes, and one node can't sit in two icons.

const PATHS = {
  play: () => <path d="M8 5.5v13a1 1 0 0 0 1.5.86l10.6-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5Z" fill="currentColor" stroke="none" />,
  pause: () => (
    <>
      <rect x="6.5" y="5" width="4" height="14" rx="1.2" fill="currentColor" stroke="none" />
      <rect x="13.5" y="5" width="4" height="14" rx="1.2" fill="currentColor" stroke="none" />
    </>
  ),
  check: () => <path d="m5 12.5 4.5 4.5L19 7.5" />,
  undo: () => <path d="M9 14 4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />,
  archive: () => (
    <>
      <rect x="3.5" y="4" width="17" height="4.5" rx="1" />
      <path d="M5.5 8.5V19a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1V8.5M10 12.5h4" />
    </>
  ),
  unarchive: () => (
    <>
      <path d="M4 13v6a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-6" />
      <path d="M12 15V4M7.5 8.5 12 4l4.5 4.5" />
    </>
  ),
  back: () => (
    <>
      <path d="M4.5 12a7.5 7.5 0 1 0 2.2-5.3M4.5 4v4h4" />
    </>
  ),
  forward: () => (
    <>
      <path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3M19.5 4v4h-4" />
    </>
  ),
  close: () => <path d="M6 6l12 12M18 6 6 18" />,
  more: () => (
    <>
      <circle cx="12" cy="5.5" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="12" cy="18.5" r="1.4" fill="currentColor" stroke="none" />
    </>
  ),
  search: () => (
    <>
      <circle cx="10.5" cy="10.5" r="6" />
      <path d="m15 15 5 5" />
    </>
  ),
  inbox: () => (
    <>
      <path d="M3.5 13.5 6 5.5a1.5 1.5 0 0 1 1.4-1h9.2a1.5 1.5 0 0 1 1.4 1l2.5 8" />
      <path d="M3.5 13.5V18a1.5 1.5 0 0 0 1.5 1.5h14a1.5 1.5 0 0 0 1.5-1.5v-4.5h-5a3.5 3.5 0 0 1-7 0h-5Z" />
    </>
  ),
  shows: () => (
    <>
      <rect x="4" y="4" width="7" height="7" rx="1.5" />
      <rect x="13" y="4" width="7" height="7" rx="1.5" />
      <rect x="4" y="13" width="7" height="7" rx="1.5" />
      <rect x="13" y="13" width="7" height="7" rx="1.5" />
    </>
  ),
  settings: () => (
    <>
      <path d="M4 7h9M17 7h3M4 17h3M11 17h9" />
      <circle cx="15" cy="7" r="2" />
      <circle cx="9" cy="17" r="2" />
    </>
  ),
  external: () => <path d="M14 4.5h5.5V10M19.5 4.5 11 13M17 14v4.5a1 1 0 0 1-1 1H5.5a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1H10" />,
  chevronDown: () => <path d="m6 9.5 6 6 6-6" />,
  arrowLeft: () => <path d="M19 12H5M11 6l-6 6 6 6" />,
  plus: () => <path d="M12 5v14M5 12h14" />,
  waves: () => <path d="M4 10v4M8 7v10M12 4v16M16 8v8M20 11v2" />,
} satisfies Record<string, () => JSX.Element>;

export type IconName = keyof typeof PATHS;

export function Icon(props: { name: IconName; size?: number }) {
  return (
    <svg
      class="icon"
      width={props.size ?? 20}
      height={props.size ?? 20}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.75"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {PATHS[props.name]()}
    </svg>
  );
}
