//! Show notes as GPUI elements: each block a styled text, lists and quotes
//! as indented columns. Links open in the system browser.

use std::ops::Range;

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveText,
    IntoElement, ParentElement as _, SharedString, Styled as _, StyledText, UnderlineStyle, div,
    px,
};

use super::{Block, LinkTarget, Run, ShowNotes};

/// The notes, top to bottom. `id` scopes the element ids of the links, so
/// two renderings in one window don't clash.
// TODO: text selection and copying. GPUI's styled text can't be selected;
// gpui-kit's TextView can, but only parses its own Markdown or HTML.
// TODO: a pointer cursor over links; `InteractiveText` has no per-range
// cursor.
pub fn render(id: impl Into<SharedString>, notes: &ShowNotes, cx: &App) -> impl IntoElement {
    let mut ids = Ids {
        prefix: id.into(),
        next: 0,
    };
    v_flex().gap_3().text_sm().children(
        notes
            .blocks
            .iter()
            .map(|block| render_block(block, &mut ids, cx)),
    )
}

/// Hands out a distinct element id per text, which `InteractiveText`
/// needs to track clicks.
struct Ids {
    prefix: SharedString,
    next: usize,
}

impl Ids {
    fn next(&mut self) -> ElementId {
        self.next += 1;
        ElementId::Name(format!("{}-{}", self.prefix, self.next).into())
    }
}

fn render_block(block: &Block, ids: &mut Ids, cx: &App) -> AnyElement {
    match block {
        Block::Paragraph(runs) => render_runs(runs, ids, cx).into_any_element(),
        Block::Heading { level, runs } => {
            let heading = div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(render_runs(runs, ids, cx));
            match level {
                1 => heading.text_xl(),
                2 => heading.text_lg(),
                _ => heading.text_base(),
            }
            .into_any_element()
        }
        Block::List { ordered, items } => v_flex()
            .gap_1()
            .children(items.iter().enumerate().map(|(index, item)| {
                let marker = if *ordered {
                    format!("{}.", index + 1)
                } else {
                    "•".to_owned()
                };
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(16.))
                            .text_color(cx.theme().muted_foreground)
                            .child(marker),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .children(item.iter().map(|block| render_block(block, ids, cx))),
                    )
            }))
            .into_any_element(),
        Block::Quote(blocks) => v_flex()
            .gap_2()
            .pl_3()
            .border_l_2()
            .border_color(cx.theme().border)
            .text_color(cx.theme().muted_foreground)
            .children(blocks.iter().map(|block| render_block(block, ids, cx)))
            .into_any_element(),
        Block::Preformatted(text) => div()
            .p_2()
            .rounded_md()
            .bg(cx.theme().muted)
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .child(text.clone())
            .into_any_element(),
    }
}

/// One text element for the runs, with the links clickable.
fn render_runs(runs: &[Run], ids: &mut Ids, cx: &App) -> impl IntoElement {
    let mut text = String::new();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut links: Vec<(Range<usize>, LinkTarget)> = Vec::new();
    for run in runs {
        let range = text.len()..text.len() + run.text.len();
        text.push_str(&run.text);
        let mut style = HighlightStyle::default();
        if run.style.bold {
            style.font_weight = Some(FontWeight::BOLD);
        }
        if run.style.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        // HighlightStyle can't switch fonts, so inline code gets the code
        // block's background instead of the mono font.
        if run.style.code {
            style.background_color = Some(cx.theme().muted);
        }
        if run.style.underline || run.link.is_some() {
            style.underline = Some(UnderlineStyle {
                thickness: px(1.),
                ..UnderlineStyle::default()
            });
        }
        if let Some(link) = &run.link {
            style.color = Some(cx.theme().link);
            links.push((range.clone(), link.clone()));
        }
        if style != HighlightStyle::default() {
            highlights.push((range, style));
        }
    }
    let (ranges, targets): (Vec<_>, Vec<_>) = links.into_iter().unzip();
    InteractiveText::new(
        ids.next(),
        StyledText::new(text).with_highlights(highlights),
    )
    .on_click(ranges, move |index, _, cx| {
        let target: &LinkTarget = &targets[index];
        cx.open_url(target.as_str());
    })
}
