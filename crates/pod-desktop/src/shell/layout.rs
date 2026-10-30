//! How the window divides its width (the design brief's "Layout by
//! width"). Wide windows spend their width on content, not margins: an
//! episode list grows date and length columns, a show's header becomes a
//! side column, and the reading pane stops at a comfortable width while the
//! list takes the rest.
//!
//! Every pane width here follows from the window's width, which the root
//! view reads each frame, so the layouts switch on the pane's own width the
//! way the web's container queries do, without measuring.

use gpui_kit::{Pixels, px};

/// The sidebar's width.
pub const SIDEBAR: Pixels = px(280.);
/// From this window width on, the reading pane keeps [`READING_PANE`] and
/// the list takes the rest.
pub const WIDE_WINDOW: Pixels = px(1680.);
/// The reading pane's width in a wide window.
pub const READING_PANE: Pixels = px(920.);
/// The narrowest the list beside an open episode gets.
pub const LIST_MIN: Pixels = px(320.);
/// Below this content width there's no room for the list and the reading
/// pane side by side, and the list fills the content.
pub const SPLIT_MIN: Pixels = px(680.);
/// Content wider than this centres instead of stretching rows edge to edge.
pub const CONTENT_MAX: Pixels = px(1600.);
/// An episode list at least this wide shows date and length as columns.
pub const COLUMNS_MIN: Pixels = px(720.);
/// A show's page at least this wide puts its header in a side column.
pub const SIDE_COLUMN_MIN: Pixels = px(1000.);
/// That side column's width.
pub const SIDE_COLUMN: Pixels = px(320.);
/// The horizontal padding of pages and list panes.
pub const GUTTER: Pixels = px(28.);

/// The widths of an episode list and the reading pane beside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Split {
    pub list: Pixels,
    /// `None` when the window is too narrow for a reading pane.
    pub detail: Option<Pixels>,
}

/// The content area's width in a window `window` wide.
pub fn content_width(window: Pixels) -> Pixels {
    (window - SIDEBAR).max(px(0.))
}

/// How a window `window` wide splits the content between the list and the
/// reading pane. Below [`WIDE_WINDOW`] the two share the width at the web's
/// 0.9 : 1.25, the list never narrower than [`LIST_MIN`].
pub fn split(window: Pixels) -> Split {
    let content = content_width(window);
    if window >= WIDE_WINDOW {
        let detail = READING_PANE.min(content - LIST_MIN);
        return Split {
            list: content - detail,
            detail: Some(detail),
        };
    }
    if content < SPLIT_MIN {
        return Split {
            list: content,
            detail: None,
        };
    }
    let list = (content * (0.9 / 2.15)).max(LIST_MIN);
    Split {
        list,
        detail: Some(content - list),
    }
}

/// Whether a list pane `list` wide shows date and length as columns. The
/// rows sit inside the pane's gutters.
pub fn has_columns(list: Pixels) -> bool {
    list.min(CONTENT_MAX) - GUTTER * 2. >= COLUMNS_MIN
}

/// Whether a show's page `pane` wide puts its header in a side column.
pub fn has_side_column(pane: Pixels) -> bool {
    pane >= SIDE_COLUMN_MIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_window_keeps_the_reading_pane_and_gives_the_list_the_rest() {
        let wide = split(px(2560.));
        assert_eq!(wide.detail, Some(READING_PANE));
        assert_eq!(wide.list, px(2560.) - SIDEBAR - READING_PANE);
        assert!(has_columns(wide.list));
        assert!(has_side_column(wide.list));

        let threshold = split(WIDE_WINDOW);
        assert_eq!(threshold.detail, Some(READING_PANE));
        assert!(threshold.list >= LIST_MIN);
    }

    #[test]
    fn a_medium_window_shares_the_width_and_keeps_the_list_usable() {
        let medium = split(px(1280.));
        let detail = medium.detail.expect("there is room for both panes");
        assert_eq!(medium.list + detail, px(1000.));
        assert!(medium.list < detail, "the episode gets the larger share");
        assert!(!has_columns(medium.list));

        let small = split(px(1000.));
        assert_eq!(small.list, LIST_MIN);
    }

    #[test]
    fn a_narrow_window_shows_the_list_alone() {
        let narrow = split(px(900.));
        assert_eq!(narrow.detail, None);
        assert_eq!(narrow.list, px(620.));
    }

    #[test]
    fn columns_follow_the_lists_own_width_and_stop_growing_with_the_content() {
        assert!(!has_columns(COLUMNS_MIN));
        assert!(has_columns(COLUMNS_MIN + GUTTER * 2.));
        assert!(has_columns(px(4000.)));
    }
}
