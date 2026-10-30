//! Pod's look: the palette and type shared with the web client (the design
//! brief's tokens), applied to gpui-kit's light and dark themes so its
//! inputs, dialogs, menus and notifications match the hand-drawn views.
//!
//! The interface stays quiet so cover art carries the colour: neutrals with
//! a little green in them and one jade accent, used only for state (what's
//! playing, how far, what's selected, primary buttons).

use std::borrow::Cow;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::{Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, Hsla, Rgba, SharedString, Window, WindowAppearance, rgb, rgba};

// ==============================================================================
// Tokens
// ==============================================================================

/// The colours every view draws with. [`palette`] picks the one for the
/// current mode.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub background: Hsla,
    /// Panels, the detail pane, buttons, inputs.
    pub surface: Hsla,
    pub sidebar: Hsla,
    pub ink: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub line: Hsla,
    /// Ink at a few percent, under a hovered row or button.
    pub hover: Hsla,
    pub accent: Hsla,
    /// Accent-coloured text, darker (light) or lighter (dark) than the
    /// accent so it reads on the background.
    pub accent_ink: Hsla,
    /// A selected row, a placeholder tile.
    pub accent_soft: Hsla,
    /// Text and icons on the accent.
    pub on_accent: Hsla,
    pub danger: Hsla,
    /// "Waiting" next to the accent's "all saved".
    pub amber: Hsla,
    /// How strongly a show's blurred cover tints the surfaces it washes.
    pub wash_opacity: f32,
    /// The colour of cover art's soft shadow.
    pub art_shadow: Hsla,
    pub is_dark: bool,
}

fn hsla(color: u32) -> Hsla {
    rgb(color).into()
}

fn hsla_alpha(color: u32) -> Hsla {
    rgba(color).into()
}

static LIGHT: LazyLock<Palette> = LazyLock::new(|| Palette {
    background: hsla(0xf4f5f1),
    surface: hsla(0xffffff),
    sidebar: hsla(0xeaece6),
    ink: hsla(0x17201b),
    muted: hsla(0x58625d),
    faint: hsla(0x89918c),
    line: hsla(0xdadfd7),
    hover: hsla_alpha(0x17201b0d),
    accent: hsla(0x0a7552),
    accent_ink: hsla(0x076142),
    accent_soft: hsla(0xd9eee3),
    on_accent: hsla(0xffffff),
    danger: hsla(0xb42318),
    amber: hsla(0xd69e2e),
    wash_opacity: 0.30,
    art_shadow: hsla_alpha(0x00000040),
    is_dark: false,
});

static DARK: LazyLock<Palette> = LazyLock::new(|| Palette {
    background: hsla(0x0f1311),
    surface: hsla(0x161b18),
    sidebar: hsla(0x0b0e0c),
    ink: hsla(0xe6ebe8),
    muted: hsla(0x9ba5a0),
    faint: hsla(0x6c7570),
    line: hsla(0x262d29),
    hover: hsla_alpha(0xe6ebe80f),
    accent: hsla(0x4fcf9b),
    accent_ink: hsla(0x79dfb6),
    accent_soft: hsla(0x163128),
    on_accent: hsla(0x052419),
    danger: hsla(0xff8a7a),
    amber: hsla(0xd69e2e),
    wash_opacity: 0.38,
    art_shadow: hsla_alpha(0x000000b3),
    is_dark: true,
});

/// The palette of the current mode.
pub fn palette(cx: &App) -> &'static Palette {
    if cx.theme().is_dark() { &DARK } else { &LIGHT }
}

// ==============================================================================
// Type
// ==============================================================================

/// UI and body text.
pub const FONT: &str = "Radio Canada";
/// Headings, show and episode titles in headers, empty-state titles.
pub const FONT_DISPLAY: &str = "Radio Canada Big";
/// The player's clocks: Radio Canada's narrow width.
pub const FONT_NARROW: &str = "Radio Canada Narrow";

/// The type scale in pixels, as the web's `--step-*`.
pub mod size {
    use gpui_kit::{Pixels, px};

    /// Meta lines, labels, headings of groups.
    pub const SMALL: Pixels = px(13.);
    /// Body and UI text.
    pub const BODY: Pixels = px(15.);
    /// Section headings in Settings, show notes.
    pub const LARGE: Pixels = px(17.);
    /// The detail pane's title, empty-state titles.
    pub const TITLE: Pixels = px(22.);
    /// Page titles.
    pub const PAGE: Pixels = px(30.);
}

/// The fonts are embedded so the app looks the same on every machine. They
/// are static, Latin-subset instances of the variable Google Fonts files
/// (`assets/fonts/OFL*.txt`): GPUI's Linux text system picks faces by family
/// and weight and doesn't drive variable axes. Other scripts fall back to
/// the system's fonts.
const FONT_FILES: [&[u8]; 9] = [
    include_bytes!("../assets/fonts/RadioCanada-Regular.ttf"),
    include_bytes!("../assets/fonts/RadioCanada-Medium.ttf"),
    include_bytes!("../assets/fonts/RadioCanada-SemiBold.ttf"),
    include_bytes!("../assets/fonts/RadioCanada-Bold.ttf"),
    include_bytes!("../assets/fonts/RadioCanada-Italic.ttf"),
    include_bytes!("../assets/fonts/RadioCanada-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/RadioCanadaNarrow-Medium.ttf"),
    include_bytes!("../assets/fonts/RadioCanadaBig-SemiBold.ttf"),
    include_bytes!("../assets/fonts/RadioCanadaBig-Bold.ttf"),
];

// ==============================================================================
// Applying the tokens to gpui-kit's themes
// ==============================================================================

/// Embeds the fonts and gives both of gpui-kit's themes Pod's palette, type
/// and radii. Requires `gpui_kit::init`.
pub fn init(cx: &mut App) {
    let fonts = FONT_FILES
        .iter()
        .map(|bytes| Cow::Borrowed(*bytes))
        .collect();
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        tracing::error!("embedding the fonts failed: {error:#}");
    }
    let theme = Theme::global(cx);
    let light = pod_config(&theme.light_theme, &LIGHT);
    let dark = pod_config(&theme.dark_theme, &DARK);
    let mode = theme.mode;
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    Theme::change(mode, None, cx);
}

/// Follows the system's light or dark appearance, now and whenever it
/// changes. `POD_APPEARANCE=light` or `dark` overrides it, for desktops
/// without an appearance setting GPUI can read (and for screenshots).
pub fn follow_appearance(window: &mut Window, cx: &mut App) {
    if let Some(mode) = forced_mode() {
        Theme::change(mode, Some(window), cx);
        return;
    }
    apply_appearance(window.appearance(), window, cx);
    window
        .observe_window_appearance(|window, cx| {
            apply_appearance(window.appearance(), window, cx);
        })
        .detach();
}

fn forced_mode() -> Option<ThemeMode> {
    match std::env::var("POD_APPEARANCE").ok()?.as_str() {
        "dark" => Some(ThemeMode::Dark),
        "light" => Some(ThemeMode::Light),
        _ => None,
    }
}

fn apply_appearance(appearance: WindowAppearance, window: &mut Window, cx: &mut App) {
    let mode = match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
    };
    if cx.theme().mode != mode {
        Theme::change(mode, Some(window), cx);
    }
}

/// `config` with Pod's tokens. Danger buttons stay solid: gpui-component
/// tints them to 20 % of the danger colour, too faint for confirming that
/// something gets deleted.
fn pod_config(config: &Rc<ThemeConfig>, palette: &Palette) -> Rc<ThemeConfig> {
    let mut config = ThemeConfig::clone(config);
    config.font_family = Some(FONT.into());
    config.font_size = Some(15.);
    config.radius = Some(8);
    config.radius_lg = Some(16);
    config.shadow = Some(false);

    let p = palette;
    let set = |slot: &mut Option<SharedString>, color: Hsla| *slot = Some(hex(color));
    let c = &mut config.colors;
    set(&mut c.background, p.background);
    set(&mut c.foreground, p.ink);
    set(&mut c.border, p.line);
    set(&mut c.input, p.line);
    set(&mut c.ring, p.accent);
    set(&mut c.caret, p.accent);
    set(&mut c.selection, p.accent.opacity(0.25));
    set(&mut c.muted, p.hover);
    set(&mut c.muted_foreground, p.muted);
    // gpui-component's "accent" is the hover tint of menu items and lists.
    set(&mut c.accent, p.hover);
    set(&mut c.accent_foreground, p.ink);
    set(&mut c.primary, p.accent);
    set(&mut c.primary_hover, mix(p.accent, p.ink, 0.12));
    set(&mut c.primary_active, mix(p.accent, p.ink, 0.2));
    set(&mut c.primary_foreground, p.on_accent);
    set(&mut c.secondary, p.surface);
    set(&mut c.secondary_hover, mix(p.surface, p.ink, 0.05));
    set(&mut c.secondary_active, mix(p.surface, p.ink, 0.08));
    set(&mut c.secondary_foreground, p.ink);
    set(&mut c.button, p.surface);
    set(&mut c.button_hover, mix(p.surface, p.ink, 0.05));
    set(&mut c.button_active, mix(p.surface, p.ink, 0.08));
    set(&mut c.button_foreground, p.ink);
    set(&mut c.button_primary, p.accent);
    set(&mut c.button_primary_hover, mix(p.accent, p.ink, 0.12));
    set(&mut c.button_primary_active, mix(p.accent, p.ink, 0.2));
    set(&mut c.button_primary_foreground, p.on_accent);
    set(&mut c.danger, p.danger);
    set(&mut c.danger_hover, mix(p.danger, p.ink, 0.12));
    set(&mut c.danger_active, mix(p.danger, p.ink, 0.2));
    set(&mut c.danger_foreground, p.background);
    set(&mut c.button_danger, p.danger);
    set(&mut c.button_danger_hover, mix(p.danger, p.ink, 0.12));
    set(&mut c.button_danger_active, mix(p.danger, p.ink, 0.2));
    set(&mut c.button_danger_foreground, p.background);
    set(&mut c.warning, p.amber);
    set(&mut c.link, p.accent_ink);
    set(&mut c.link_hover, p.accent_ink);
    set(&mut c.link_active, p.accent_ink);
    set(&mut c.popover, p.surface);
    set(&mut c.popover_foreground, p.ink);
    set(&mut c.list, p.background);
    set(&mut c.list_hover, p.hover);
    set(&mut c.list_active, p.accent_soft);
    set(&mut c.list_active_border, p.accent_soft);
    set(&mut c.sidebar, p.sidebar);
    set(&mut c.sidebar_foreground, p.ink);
    set(&mut c.sidebar_border, p.sidebar);
    set(&mut c.sidebar_accent, p.surface);
    set(&mut c.sidebar_accent_foreground, p.ink);
    set(&mut c.sidebar_primary, p.accent);
    set(&mut c.sidebar_primary_foreground, p.on_accent);
    set(&mut c.title_bar, p.background);
    set(&mut c.title_bar_border, p.background);
    set(&mut c.switch, p.line);
    set(&mut c.switch_thumb, p.surface);
    set(&mut c.progress_bar, p.accent);
    set(&mut c.slider_bar, p.accent);
    set(&mut c.slider_thumb, p.ink);
    set(&mut c.skeleton, p.hover);
    set(&mut c.scrollbar, Hsla::transparent_black());
    set(&mut c.scrollbar_thumb, p.ink.opacity(0.18));
    set(&mut c.scrollbar_thumb_hover, p.ink.opacity(0.3));
    set(&mut c.overlay, hsla_alpha(0x080c0a73));
    set(&mut c.window_border, p.line);
    Rc::new(config)
}

/// `a` with `amount` of `b` mixed in, as CSS `color-mix(in srgb, …)`.
pub fn mix(a: Hsla, b: Hsla, amount: f32) -> Hsla {
    let (a, b) = (Rgba::from(a), Rgba::from(b));
    let lerp = |x: f32, y: f32| x + (y - x) * amount;
    Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
    .into()
}

fn hex(color: Hsla) -> SharedString {
    let Rgba { r, g, b, a } = color.into();
    let byte = |channel: f32| (channel.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(r),
        byte(g),
        byte(b),
        byte(a)
    )
    .into()
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;
    use gpui_kit::component::ActiveTheme as _;
    use gpui_kit::component::theme::{Theme, ThemeMode};

    /// The hex round trip can move a channel by a fraction of a step.
    fn near(a: gpui_kit::Hsla, b: gpui_kit::Hsla) -> bool {
        let (a, b) = (gpui_kit::Rgba::from(a), gpui_kit::Rgba::from(b));
        [a.r - b.r, a.g - b.g, a.b - b.b, a.a - b.a]
            .iter()
            .all(|delta| delta.abs() < 1. / 255.)
    }

    #[gpui_kit::test]
    fn both_modes_carry_the_palette_and_keep_it_across_switches(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::init(cx);
            for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Light] {
                Theme::change(mode, None, cx);
                let palette = super::palette(cx);
                assert_eq!(palette.is_dark, mode == ThemeMode::Dark);
                let theme = cx.theme();
                assert!(near(theme.background, palette.background), "{mode:?}");
                assert!(near(theme.primary, palette.accent), "{mode:?}");
                assert!(near(theme.link, palette.accent_ink), "{mode:?}");
                assert!(
                    near(theme.button_danger, theme.danger),
                    "{mode:?}: danger buttons are solid"
                );
                assert_eq!(theme.font_family.as_ref(), super::FONT);
            }
        });
    }

    #[test]
    fn mix_blends_in_srgb() {
        let black = gpui_kit::Hsla::black();
        let white = gpui_kit::Hsla::white();
        let grey = gpui_kit::Rgba::from(super::mix(black, white, 0.5));
        assert!((grey.r - 0.5).abs() < 1e-4 && (grey.a - 1.).abs() < 1e-4);
    }
}
