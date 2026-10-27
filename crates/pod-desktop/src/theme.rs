//! Pod's changes to gpui-kit's default light and dark themes.

use std::rc::Rc;

use gpui_kit::component::theme::{Theme, ThemeConfig};
use gpui_kit::{App, Hsla, Rgba, SharedString};

/// Makes danger buttons solid. gpui-component tints them to 20% of the
/// danger colour, too faint for confirming that something gets deleted,
/// and a per-button `bg` loses to its hover style. The change goes into
/// both modes' configs, so switching between light and dark keeps it.
/// Requires `gpui_kit::init`.
pub fn init(cx: &mut App) {
    let theme = Theme::global(cx);
    let light = solid_danger_buttons(&theme.light_theme);
    let dark = solid_danger_buttons(&theme.dark_theme);
    let mode = theme.mode;
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    Theme::change(mode, None, cx);
}

fn solid_danger_buttons(config: &Rc<ThemeConfig>) -> Rc<ThemeConfig> {
    // The danger colours themselves are often fallbacks derived from
    // others, so they're read off the config as applied.
    let mut applied = Theme::default();
    applied.apply_config(config);
    let mut config = ThemeConfig::clone(config);
    let colors = &mut config.colors;
    colors.button_danger = Some(hex(applied.danger));
    colors.button_danger_hover = Some(hex(applied.danger_hover));
    colors.button_danger_active = Some(hex(applied.danger_active));
    colors.button_danger_foreground = Some(hex(applied.danger_foreground));
    Rc::new(config)
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

    #[gpui_kit::test]
    fn danger_buttons_are_solid_in_both_modes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::init(cx);
            for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Light] {
                Theme::change(mode, None, cx);
                let theme = cx.theme();
                // The hex round trip can move a channel by a fraction of a
                // step.
                let near = |a: gpui_kit::Hsla, b: gpui_kit::Hsla| {
                    let (a, b) = (gpui_kit::Rgba::from(a), gpui_kit::Rgba::from(b));
                    [a.r - b.r, a.g - b.g, a.b - b.b, a.a - b.a]
                        .iter()
                        .all(|delta| delta.abs() < 1. / 255.)
                };
                assert!(
                    near(theme.button_danger, theme.danger),
                    "{mode:?}: {:?} is the danger colour {:?}",
                    theme.button_danger,
                    theme.danger
                );
                assert!(
                    near(theme.button_danger_hover, theme.danger_hover),
                    "{mode:?}"
                );
                assert!(
                    near(theme.button_danger_active, theme.danger_active),
                    "{mode:?}"
                );
                assert!(
                    near(theme.button_danger_foreground, theme.danger_foreground),
                    "{mode:?}"
                );
            }
        });
    }
}
