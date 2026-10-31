//! Pod's icons, the web client's set (`web/src/ui/Icon.tsx`): a 24 px grid
//! with round 1.75 px strokes so they sit beside Radio Canada's text weight,
//! and filled play and pause shapes that read at a glance in small buttons.
//!
//! GPUI draws an SVG icon as a mask in the text colour, so each shape is
//! drawn in plain black here and takes its colour from where it's placed.
//! The asset source serves them under [`PREFIX`].

use gpui_kit::SharedString;
use gpui_kit::assets::IconNamed;

/// Where [`crate::assets::AppAssets`] serves the icons.
pub const PREFIX: &str = "pod-icons/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PodIcon {
    Play,
    Pause,
    Check,
    Undo,
    Archive,
    Unarchive,
    SkipBack,
    SkipForward,
    Close,
    More,
    Search,
    Inbox,
    Shows,
    Settings,
    External,
    ChevronDown,
    Plus,
    Minus,
    Waves,
    Download,
    Refresh,
    /// A speaker, for the output picker (spec 13.4).
    Speaker,
    /// The logo's pod, on the accent square.
    LogoPod,
    /// The logo's peas, cut out of the pod in the accent colour.
    LogoPeas,
}

impl PodIcon {
    pub const ALL: [PodIcon; 24] = [
        Self::Play,
        Self::Pause,
        Self::Check,
        Self::Undo,
        Self::Archive,
        Self::Unarchive,
        Self::SkipBack,
        Self::SkipForward,
        Self::Close,
        Self::More,
        Self::Search,
        Self::Inbox,
        Self::Shows,
        Self::Settings,
        Self::External,
        Self::ChevronDown,
        Self::Plus,
        Self::Minus,
        Self::Waves,
        Self::Download,
        Self::Refresh,
        Self::Speaker,
        Self::LogoPod,
        Self::LogoPeas,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Check => "check",
            Self::Undo => "undo",
            Self::Archive => "archive",
            Self::Unarchive => "unarchive",
            Self::SkipBack => "skip-back",
            Self::SkipForward => "skip-forward",
            Self::Close => "close",
            Self::More => "more",
            Self::Search => "search",
            Self::Inbox => "inbox",
            Self::Shows => "shows",
            Self::Settings => "settings",
            Self::External => "external",
            Self::ChevronDown => "chevron-down",
            Self::Plus => "plus",
            Self::Minus => "minus",
            Self::Waves => "waves",
            Self::Download => "download",
            Self::Refresh => "refresh",
            Self::Speaker => "speaker",
            Self::LogoPod => "logo-pod",
            Self::LogoPeas => "logo-peas",
        }
    }

    /// The shapes inside the `<svg>`.
    fn shapes(self) -> &'static str {
        match self {
            Self::Play => {
                r#"<path d="M8 5.5v13a1 1 0 0 0 1.5.86l10.6-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5Z" fill="black" stroke="none"/>"#
            }
            Self::Pause => {
                r#"<rect x="6.5" y="5" width="4" height="14" rx="1.2" fill="black" stroke="none"/><rect x="13.5" y="5" width="4" height="14" rx="1.2" fill="black" stroke="none"/>"#
            }
            Self::Check => r#"<path d="m5 12.5 4.5 4.5L19 7.5"/>"#,
            Self::Undo => r#"<path d="M9 14 4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>"#,
            Self::Archive => {
                r#"<rect x="3.5" y="4" width="17" height="4.5" rx="1"/><path d="M5.5 8.5V19a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1V8.5M10 12.5h4"/>"#
            }
            Self::Unarchive => {
                r#"<path d="M4 13v6a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-6"/><path d="M12 15V4M7.5 8.5 12 4l4.5 4.5"/>"#
            }
            Self::SkipBack => r#"<path d="M4.5 12a7.5 7.5 0 1 0 2.2-5.3M4.5 4v4h4"/>"#,
            Self::SkipForward => r#"<path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3M19.5 4v4h-4"/>"#,
            Self::Close => r#"<path d="M6 6l12 12M18 6 6 18"/>"#,
            Self::More => {
                r#"<circle cx="12" cy="5.5" r="1.4" fill="black" stroke="none"/><circle cx="12" cy="12" r="1.4" fill="black" stroke="none"/><circle cx="12" cy="18.5" r="1.4" fill="black" stroke="none"/>"#
            }
            Self::Search => r#"<circle cx="10.5" cy="10.5" r="6"/><path d="m15 15 5 5"/>"#,
            Self::Inbox => {
                r#"<path d="M3.5 13.5 6 5.5a1.5 1.5 0 0 1 1.4-1h9.2a1.5 1.5 0 0 1 1.4 1l2.5 8"/><path d="M3.5 13.5V18a1.5 1.5 0 0 0 1.5 1.5h14a1.5 1.5 0 0 0 1.5-1.5v-4.5h-5a3.5 3.5 0 0 1-7 0h-5Z"/>"#
            }
            Self::Shows => {
                r#"<rect x="4" y="4" width="7" height="7" rx="1.5"/><rect x="13" y="4" width="7" height="7" rx="1.5"/><rect x="4" y="13" width="7" height="7" rx="1.5"/><rect x="13" y="13" width="7" height="7" rx="1.5"/>"#
            }
            Self::Settings => {
                r#"<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>"#
            }
            Self::External => {
                r#"<path d="M14 4.5h5.5V10M19.5 4.5 11 13M17 14v4.5a1 1 0 0 1-1 1H5.5a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1H10"/>"#
            }
            Self::ChevronDown => r#"<path d="m6 9.5 6 6 6-6"/>"#,
            Self::Plus => r#"<path d="M12 5v14M5 12h14"/>"#,
            Self::Minus => r#"<path d="M5 12h14"/>"#,
            Self::Waves => r#"<path d="M4 10v4M8 7v10M12 4v16M16 8v8M20 11v2"/>"#,
            // Not in the web's set: the desktop has downloads. Drawn in the
            // same hand as the archive box.
            Self::Download => r#"<path d="M12 4v11M7.5 10.5 12 15l4.5-4.5M4.5 19.5h15"/>"#,
            Self::Refresh => r#"<path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3M19.5 4v4h-4"/>"#,
            Self::Speaker => {
                r#"<rect x="5.5" y="3" width="13" height="18" rx="2.5"/><circle cx="12" cy="14.25" r="3.5"/><circle cx="12" cy="7.25" r="1.1" fill="black" stroke="none"/>"#
            }
            Self::LogoPod => {
                r#"<path d="M5.5 26.5C6 15 13 6.5 26.5 5.5C26 17 18 25.5 5.5 26.5Z" fill="black" stroke="none"/>"#
            }
            Self::LogoPeas => {
                r#"<circle cx="11.4" cy="20.6" r="2.7" fill="black" stroke="none"/><circle cx="16" cy="16" r="2.9" fill="black" stroke="none"/><circle cx="20.6" cy="11.4" r="2.7" fill="black" stroke="none"/>"#
            }
        }
    }

    /// The whole SVG file.
    pub fn svg(self) -> String {
        let view_box = match self {
            Self::LogoPod | Self::LogoPeas => "0 0 32 32",
            _ => "0 0 24 24",
        };
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view_box}" fill="none" stroke="black" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
            self.shapes()
        )
    }

    /// The icon an asset path names, if it is one of these.
    pub fn from_path(path: &str) -> Option<PodIcon> {
        let name = path.strip_prefix(PREFIX)?.strip_suffix(".svg")?;
        Self::ALL.into_iter().find(|icon| icon.name() == name)
    }
}

impl IconNamed for PodIcon {
    fn path(self) -> SharedString {
        format!("{PREFIX}{}.svg", self.name()).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_round_trips_through_its_path_and_parses() {
        for icon in PodIcon::ALL {
            assert_eq!(PodIcon::from_path(&icon.path()), Some(icon));
            let svg = icon.svg();
            assert!(
                svg.starts_with("<svg") && svg.ends_with("</svg>"),
                "{icon:?}"
            );
        }
    }
}
