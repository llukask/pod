# Pod visual design

The look shared by the three clients. The web client (`web/`) came first
and is the reference implementation: `web/src/styles.css` holds every
token and measurement, `web/src/ui/*.tsx` the structure. The desktop
client follows it in `crates/pod-desktop/src/theme.rs` and `shell/`, the
Android client in `android/app/src/main/kotlin/ltd/kilian/pod/ui/`
(`Theme.kt`, `Kit.kt`). Platforms differ where their idioms do: phones
get a tab bar and a player sheet, the desktop no phone layouts.

## Principles

1. The interface stays quiet so cover art carries the colour. Neutrals
   with a little green in them, one jade accent used only for state:
   what's playing, how far you are, what's selected, primary buttons.
2. Each show's cover, blown up and heavily blurred, washes the surfaces
   that belong to it: the player bar, the mini player, the full player
   sheet, and the top of a show's page (fading into the background).
   This is the one bold move; everything else is restrained.
3. Every episode's play button carries a progress ring: a thin circle
   around the button filling clockwise from 12 o'clock with how much has
   been heard (accent colour; faint grey once played; a started episode
   shows at least ~6 % so it doesn't read as a stray dot; the current
   episode's ring follows the player live). The current episode's
   button is filled with the accent.
4. Wide screens use their width for content, not margins (see Layout).
   Phones get thumb-sized, native-feeling navigation.

## Tokens

Light:
- background `#f4f5f1`, surface `#ffffff`, sidebar/tab bar `#eaece6`
- ink `#17201b`, muted `#58625d`, faint `#89918c`, line `#dadfd7`
- hover: ink at 5 %
- accent `#0a7552`, accent ink (accent-coloured text) `#076142`,
  accent soft (selected row, placeholder tile) `#d9eee3`, on-accent `#ffffff`
- danger `#b42318`
- ambient wash opacity 0.30

Dark:
- background `#0f1311`, surface `#161b18`, sidebar `#0b0e0c`
- ink `#e6ebe8`, muted `#9ba5a0`, faint `#6c7570`, line `#262d29`
- hover: ink at 6 %
- accent `#4fcf9b`, accent ink `#79dfb6`, accent soft `#163128`,
  on-accent `#052419`
- danger `#ff8a7a`
- ambient wash opacity 0.38 (mini player +0.15, full sheet +0.25)

Radii: 8 (inputs, small art), 12 (rows, panels, menus), 16-20
(dialogs, sheet cover), fully round (buttons are pills, play buttons
circles). Cover art 6-12 px radius by size, with a soft shadow at
large sizes.

## Type

Radio Canada (variable, wght 300-700, wdth 75-100) for UI and body;
Radio Canada Big (wght) for headings, show and episode titles in
headers, the sheet title, empty-state titles. Both are OFL, on Google
Fonts (`github.com/google/fonts`, `ofl/radiocanada`,
`ofl/radiocanadabig`). The web bundles them from
`web/node_modules/@fontsource-variable/radio-canada{,-big}` (woff2
only); native apps need TTF: fetch from Google Fonts' repo if the
network allows, or convert the woff2 files (e.g. `woff2_decompress`
from `nix shell nixpkgs#woff2`). Use static instances if the platform
can't drive variable axes.

Scale (rem = 16 px): 13 (meta, labels), 15 (body/UI), 17, 22 (detail
title), 30 (page title, 28 on phones), 40. Headings 650-700 weight with
-0.015 to -0.025 em tracking; body 400, titles in rows 600 (played rows
450 and muted). Tabular numerals for times and durations. Sentence case
everywhere; no all-caps labels.

## Brand

Logo: a pea pod (the app is called Pod). 32x32 viewBox:

    <rect width="32" height="32" rx="8" fill="ACCENT"/>
    <path d="M5.5 26.5C6 15 13 6.5 26.5 5.5C26 17 18 25.5 5.5 26.5Z" fill="ON_ACCENT"/>
    <circle cx="11.4" cy="20.6" r="2.7" fill="ACCENT"/>
    <circle cx="16" cy="16" r="2.9" fill="ACCENT"/>
    <circle cx="20.6" cy="11.4" r="2.7" fill="ACCENT"/>

Light accent `#0a7552`, on-accent white (`web/public/favicon.svg`).

Icons: 24 px grid, 1.75 px round strokes, filled play/pause; the full
set with paths is `web/src/ui/Icon.tsx` (play, pause, check, undo,
archive, unarchive, skip back/forward as circular arrows with the
seconds printed inside, close, more, search, inbox, shows (2x2 grid),
settings (sliders), external, chevron down, arrow left, plus, waves).
Use the platform's icon mechanism but these shapes.

## Components

- Episode row: [cover 52 px, inbox only] [show name 13 px muted /
  title 600 weight, up to 2 lines / meta 13 px muted "Oct 21 · 43 min"
  or "· 21 min left" or "· Played"] [hover actions: mark played,
  archive, desktop only] [play button with ring, 42 px]. Rows separated
  by hairlines inset 12 px; hovered row hover-tinted, selected row
  accent-soft, current (playing) episode's title in accent ink.
- Inbox grouped by day: "Today", "Yesterday", "This week" (the six
  days before), "Earlier"; headings 13 px semibold muted, sticky while
  their rows scroll. Rows under Today/Yesterday omit the date.
- Placeholder art (no cover): accent-soft tile with the show's initial
  in Radio Canada Big bold, accent-ink, ~42 % of the tile; skip a
  leading "The"; a sound-wave glyph when there's no title yet.
- Episode detail: cover 112 px with shadow, show name as an accent-ink
  link, title in Radio Canada Big 22 px, meta; actions: primary pill
  "Play" / "Pause" / "Resume · 21 min left" with icon, then pill
  buttons with icons "Mark played", "Archive", and a quiet "Episode
  page"; a hairline, then show notes at 16 px / 1.65 line height,
  max ~66 characters wide.
- Show header: cover 160 px with shadow, title Radio Canada Big 30 px,
  author muted, a "✓ Subscribed" pill and a ⋮ menu (Unsubscribe),
  description clamped to 2 lines with "More about this show" only when
  it actually overflows. The blurred cover washes the top ~480 px and
  fades out. All / Unplayed as a segmented control.
- Player bar (wide): cover 52 + title/show | skip back, round play
  (ink fill, 44 px), skip forward, scrubber below (thin 4 px track,
  accent fill, thumb grows on hover; clocks 12 px narrow tabular) |
  speed pill "1.2×". Blurred cover behind. Hidden when nothing loaded.
- Mini player (phones): floating rounded card (radius 14, shadow)
  above the tab bar: cover 44, title/show, pause/play; a 2 px accent
  progress line along the bottom; blurred cover behind; tap opens the
  full player sheet: chevron down to close, cover as large as fits,
  title (Radio Canada Big 22) and show, big scrubber with clocks below
  it, skip back / 76 px play / skip forward, speed pill. Strong
  blurred-cover background.
- Tab bar (phones): Inbox, Podcasts, Search, Settings; icon above a
  11 px label; active tab ink with accent icon.
- Podcasts grid: covers with shadow, title (2 lines) and author, ending
  in an "Add a podcast" tile with a dashed border and a plus.
- Settings: grouped panels (surface, hairline borders, radius 12) with
  rows; a coloured dot for "All saved to the server" (accent) or
  waiting (amber).
- Empty states: an icon, a Radio Canada Big title ("You're all caught
  up"), one muted line, and an action ("Add a podcast").
- Sign-in: centred card with the logo, "Sign in to Pod", the server.

## Layout by width

- Narrow (phones): tab bar, mini player + full sheet, an open episode
  is its own page with a back affordance, full-width primary Play.
- Medium: sidebar (280 px: logo + "Pod", search, Inbox, Settings, a
  "Podcasts" heading that opens the grid, then the show list with
  28 px covers) beside content; player bar along the bottom.
- Wide content panes switch on the pane's own width, not the window's,
  because a list is narrower when an episode is open beside it:
  - Episode list >= 720 px: date and length move into aligned columns
    (6.5 rem and 8.5 rem) right of the title, like a track list.
  - Show page pane >= 1000 px: the header becomes a sticky 320 px side
    column (cover full column width, title 22 px, description up to 8
    lines) beside the episode list.
  - Window >= 1680 px: the inbox keeps a reading pane open; with no
    episode chosen it shows the newest one (without navigation and
    without a close button), and marking it played or archiving it
    moves on to the next.
  - An open episode in a wide window gets a ~920 px reading pane; the
    list takes the rest.
  - Very wide: content centres at 1600 px; the covers grid isn't
    capped.
