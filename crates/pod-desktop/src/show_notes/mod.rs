//! Show notes: an episode's sanitized HTML (spec 2.2) as a small tree of
//! blocks and styled text runs, which [`render`] turns into GPUI elements.
//!
//! The server already sanitized the HTML, but the client still treats it as
//! untrusted (spec 2.2). Parsing keeps the allowlisted tags and nothing else:
//! any other element contributes only its text, and a link survives only if
//! its target is one the system browser may open ([`LinkTarget`]).

pub mod render;

use std::fmt;

use html5ever::Attribute;
use html5ever::tendril::TendrilSink as _;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// Parsed show notes, top to bottom.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShowNotes {
    pub blocks: Vec<Block>,
}

/// A block-level piece of the notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A run of text: a `p`, or loose text between blocks. Line breaks
    /// (`br`) are `\n` inside the runs.
    Paragraph(Vec<Run>),
    /// `h1` to `h6`; `level` is 1 to 6.
    Heading {
        level: u8,
        runs: Vec<Run>,
    },
    /// `ul` or `ol`. Each item is a list of blocks, so an item can hold
    /// several paragraphs or a nested list.
    List {
        ordered: bool,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    /// `pre`, whitespace kept verbatim.
    Preformatted(String),
}

/// Text with one style. Adjacent runs always differ in style or link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: RunStyle,
    pub link: Option<LinkTarget>,
}

/// The inline tags of the allowlist: `strong`/`b`, `em`/`i`, `u`, `code`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub code: bool,
}

/// A link target the system browser may open: an `http`, `https` or
/// `mailto` URL. Anything else (`javascript:`, `file:`, a relative path)
/// doesn't parse, and its link text renders as plain text.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkTarget(String);

impl LinkTarget {
    pub fn parse(href: &str) -> Option<LinkTarget> {
        // The URL goes to the OS launcher as it is, so whitespace and
        // control characters, which a browser would strip or a launcher
        // might split on, rule it out rather than being cleaned up.
        if href.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return None;
        }
        let (scheme, rest) = href.split_once(':')?;
        let openable = match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => rest.len() > 2 && rest.starts_with("//"),
            "mailto" => !rest.is_empty(),
            _ => false,
        };
        openable.then(|| LinkTarget(href.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LinkTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.0)
    }
}

// ==============================================================================
// Parsing
// ==============================================================================

/// Parses sanitized show-notes HTML. Never fails: HTML has no syntax
/// errors, only recoveries, and html5ever applies the browsers' ones.
pub fn parse(html: &str) -> ShowNotes {
    let dom = html5ever::parse_document(RcDom::default(), Default::default()).one(html);
    let mut flow = Flow::default();
    walk(&dom.document, &mut flow, &Inline::default());
    ShowNotes {
        blocks: flow.finish(),
    }
}

/// Elements outside the allowlist that still separate paragraphs, so a
/// `div` or a table row doesn't run into the text around it. Every other
/// unknown element is transparent: its text joins the current paragraph.
const PARAGRAPH_BOUNDARIES: &[&str] = &[
    "address",
    "article",
    "aside",
    "caption",
    "center",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hr",
    "iframe",
    "main",
    "nav",
    "section",
    "summary",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "tr",
];

/// The inline formatting in effect at a point of the walk.
#[derive(Debug, Clone, Default)]
struct Inline {
    style: RunStyle,
    link: Option<LinkTarget>,
}

/// Collects one container's blocks (the document, a list item, a quote).
/// Inline content goes into an open paragraph, which the next block, or
/// the end of the container, closes.
#[derive(Debug, Default)]
struct Flow {
    blocks: Vec<Block>,
    runs: Vec<Run>,
    /// Set while inside `h1` to `h6`: the open paragraph closes as a heading.
    heading: Option<u8>,
}

impl Flow {
    /// Appends text with HTML's whitespace rules: every run of whitespace
    /// is one space, and none starts a line.
    fn text(&mut self, raw: &str, inline: &Inline) {
        let mut collapsed = String::with_capacity(raw.len());
        let mut pending_space = false;
        for c in raw.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C') {
                pending_space = true;
            } else {
                if pending_space {
                    collapsed.push(' ');
                    pending_space = false;
                }
                collapsed.push(c);
            }
        }
        if pending_space {
            collapsed.push(' ');
        }
        let at_line_start = self
            .runs
            .last()
            .and_then(|run| run.text.chars().last())
            .is_none_or(|last| last == ' ' || last == '\n');
        let text = if at_line_start {
            collapsed.trim_start_matches(' ')
        } else {
            &collapsed
        };
        self.text_run(text, inline);
    }

    /// Appends `text` as it is, to the last run if it has the same style.
    fn text_run(&mut self, text: &str, inline: &Inline) {
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(last) if last.style == inline.style && last.link == inline.link => {
                last.text.push_str(text);
            }
            _ => self.runs.push(Run {
                text: text.to_owned(),
                style: inline.style,
                link: inline.link.clone(),
            }),
        }
    }

    /// A `br`. One at the start of a paragraph has nothing to break. The
    /// break is unstyled, so it never extends a link or an underline.
    fn line_break(&mut self) {
        self.trim_end(&[' ']);
        if !self.runs.is_empty() {
            self.text_run("\n", &Inline::default());
        }
    }

    fn trim_end(&mut self, chars: &[char]) {
        while let Some(last) = self.runs.last_mut() {
            let kept = last.text.trim_end_matches(chars).len();
            last.text.truncate(kept);
            if !last.text.is_empty() {
                break;
            }
            self.runs.pop();
        }
    }

    fn end_paragraph(&mut self) {
        self.trim_end(&[' ', '\n']);
        let runs = std::mem::take(&mut self.runs);
        let heading = self.heading.take();
        if runs.is_empty() {
            return;
        }
        self.blocks.push(match heading {
            Some(level) => Block::Heading { level, runs },
            None => Block::Paragraph(runs),
        });
    }

    fn push_block(&mut self, block: Block) {
        self.end_paragraph();
        self.blocks.push(block);
    }

    fn finish(mut self) -> Vec<Block> {
        self.end_paragraph();
        self.blocks
    }
}

fn walk(node: &Handle, flow: &mut Flow, inline: &Inline) {
    match &node.data {
        NodeData::Document => walk_children(node, flow, inline),
        NodeData::Text { contents } => flow.text(&contents.borrow(), inline),
        NodeData::Element { name, attrs, .. } => {
            element(&name.local, &attrs.borrow(), node, flow, inline);
        }
        NodeData::Doctype { .. }
        | NodeData::Comment { .. }
        | NodeData::ProcessingInstruction { .. } => {}
    }
}

fn walk_children(node: &Handle, flow: &mut Flow, inline: &Inline) {
    for child in node.children.borrow().iter() {
        walk(child, flow, inline);
    }
}

fn element(tag: &str, attrs: &[Attribute], node: &Handle, flow: &mut Flow, inline: &Inline) {
    let styled = |change: fn(&mut RunStyle)| {
        let mut inner = inline.clone();
        change(&mut inner.style);
        inner
    };
    if let Some(level) = heading_level(tag) {
        flow.end_paragraph();
        flow.heading = Some(level);
        walk_children(node, flow, inline);
        flow.end_paragraph();
        return;
    }
    match tag {
        // `li` outside a list is just a paragraph.
        "p" | "li" => {
            flow.end_paragraph();
            walk_children(node, flow, inline);
            flow.end_paragraph();
        }
        "br" => flow.line_break(),
        "ul" | "ol" => {
            let items = list_items(node, inline);
            if items.is_empty() {
                flow.end_paragraph();
            } else {
                flow.push_block(Block::List {
                    ordered: tag == "ol",
                    items,
                });
            }
        }
        "blockquote" => {
            let mut inner = Flow::default();
            walk_children(node, &mut inner, inline);
            let blocks = inner.finish();
            flow.end_paragraph();
            if !blocks.is_empty() {
                flow.push_block(Block::Quote(blocks));
            }
        }
        "pre" => {
            let mut text = String::new();
            verbatim_text(node, &mut text);
            flow.end_paragraph();
            if !text.trim().is_empty() {
                flow.push_block(Block::Preformatted(text.trim_end_matches('\n').to_owned()));
            }
        }
        "strong" | "b" => walk_children(node, flow, &styled(|style| style.bold = true)),
        "em" | "i" => walk_children(node, flow, &styled(|style| style.italic = true)),
        "u" => walk_children(node, flow, &styled(|style| style.underline = true)),
        "code" => walk_children(node, flow, &styled(|style| style.code = true)),
        "a" => {
            let link = attrs
                .iter()
                .find(|attr| &*attr.name.local == "href")
                .and_then(|attr| LinkTarget::parse(&attr.value));
            let inner = Inline {
                link,
                ..inline.clone()
            };
            walk_children(node, flow, &inner);
        }
        // Code and markup that were never meant to be read. The sanitizer
        // drops the text inside `script` and `style` too (spec 2.2).
        "head" | "script" | "style" | "template" => {}
        // Cells of a row read as one line, separated by spaces.
        "td" | "th" => {
            flow.text(" ", inline);
            walk_children(node, flow, inline);
            flow.text(" ", inline);
        }
        _ if PARAGRAPH_BOUNDARIES.contains(&tag) => {
            flow.end_paragraph();
            walk_children(node, flow, inline);
            flow.end_paragraph();
        }
        _ => walk_children(node, flow, inline),
    }
}

fn heading_level(tag: &str) -> Option<u8> {
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

/// The blocks of each item of a `ul` or `ol`. Content outside any `li`
/// joins the item before it, or starts one if there is none. Empty items
/// are dropped.
fn list_items(list: &Handle, inline: &Inline) -> Vec<Vec<Block>> {
    let mut items: Vec<Flow> = Vec::new();
    for child in list.children.borrow().iter() {
        let is_item = matches!(&child.data, NodeData::Element { name, .. } if &*name.local == "li");
        if is_item || items.is_empty() {
            items.push(Flow::default());
        }
        let item = items
            .last_mut()
            .expect("an item was pushed above if there was none");
        if is_item {
            walk_children(child, item, inline);
        } else {
            walk(child, item, inline);
        }
    }
    items
        .into_iter()
        .map(Flow::finish)
        .filter(|blocks| !blocks.is_empty())
        .collect()
}

/// The text under a `pre`, whitespace and all.
fn verbatim_text(node: &Handle, out: &mut String) {
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => out.push_str(&contents.borrow()),
            NodeData::Element { name, .. } => match &*name.local {
                "br" => out.push('\n'),
                "script" | "style" | "template" => {}
                _ => verbatim_text(child, out),
            },
            _ => {}
        }
    }
}

/// The notes as one line of plain text, blocks joined by spaces, for a
/// clamped summary such as a show's description in its header.
pub fn plain_text(notes: &ShowNotes) -> String {
    fn push_blocks(blocks: &[Block], out: &mut Vec<String>) {
        for block in blocks {
            match block {
                Block::Paragraph(runs) | Block::Heading { runs, .. } => {
                    out.push(runs.iter().map(|run| run.text.as_str()).collect());
                }
                Block::List { items, .. } => {
                    for item in items {
                        push_blocks(item, out);
                    }
                }
                Block::Quote(blocks) => push_blocks(blocks, out),
                Block::Preformatted(text) => out.push(text.clone()),
            }
        }
    }
    let mut parts = Vec::new();
    push_blocks(&notes.blocks, &mut parts);
    parts
        .iter()
        .flat_map(|part| part.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// The tree as indented text, one run per line, for snapshots.
    fn outline(notes: &ShowNotes) -> String {
        fn blocks(out: &mut String, list: &[Block], depth: usize) {
            for block in list {
                let indent = "  ".repeat(depth);
                match block {
                    Block::Paragraph(text) => {
                        writeln!(out, "{indent}paragraph").expect("writing to a String works");
                        runs(out, text, depth + 1);
                    }
                    Block::Heading { level, runs: text } => {
                        writeln!(out, "{indent}h{level}").expect("writing to a String works");
                        runs(out, text, depth + 1);
                    }
                    Block::List { ordered, items } => {
                        let kind = if *ordered { "ordered" } else { "bulleted" };
                        writeln!(out, "{indent}{kind} list").expect("writing to a String works");
                        for item in items {
                            writeln!(out, "{indent}  item").expect("writing to a String works");
                            blocks(out, item, depth + 2);
                        }
                    }
                    Block::Quote(inner) => {
                        writeln!(out, "{indent}quote").expect("writing to a String works");
                        blocks(out, inner, depth + 1);
                    }
                    Block::Preformatted(text) => {
                        writeln!(out, "{indent}preformatted").expect("writing to a String works");
                        for line in text.lines() {
                            writeln!(out, "{indent}  | {line}").expect("writing to a String works");
                        }
                    }
                }
            }
        }
        fn runs(out: &mut String, runs: &[Run], depth: usize) {
            for run in runs {
                let RunStyle {
                    bold,
                    italic,
                    underline,
                    code,
                } = run.style;
                let mut marks: Vec<String> = [
                    (bold, "bold"),
                    (italic, "italic"),
                    (underline, "underline"),
                    (code, "code"),
                ]
                .into_iter()
                .filter(|(on, _)| *on)
                .map(|(_, name)| name.to_owned())
                .collect();
                if let Some(link) = &run.link {
                    marks.push(format!("{link:?}"));
                }
                marks.push(format!("{:?}", run.text));
                writeln!(out, "{}{}", "  ".repeat(depth), marks.join(" "))
                    .expect("writing to a String works");
            }
        }
        let mut out = String::new();
        blocks(&mut out, &notes.blocks, 0);
        out
    }

    fn plain(text: &str) -> Run {
        Run {
            text: text.to_owned(),
            style: RunStyle::default(),
            link: None,
        }
    }

    #[test]
    fn only_web_and_mail_links_open() {
        for href in [
            "https://example.com/episode?id=1#notes",
            "http://example.com",
            "HTTPS://EXAMPLE.COM",
            "mailto:tips@example.com",
        ] {
            assert_eq!(
                LinkTarget::parse(href).map(|target| target.as_str().to_owned()),
                Some(href.to_owned()),
                "{href}"
            );
        }
        for href in [
            "javascript:alert(1)",
            " javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "ftp://files.example.com/a.mp3",
            "/episodes/412",
            "example.com",
            "https:",
            "mailto:",
            "https://exa mple.com",
            "https://example.com/\nfile:///etc/passwd",
            "",
        ] {
            assert_eq!(LinkTarget::parse(href), None, "{href:?}");
        }
    }

    #[test]
    fn whitespace_collapses_like_html() {
        assert_eq!(
            parse("  <p>  one\n\t two  </p>\n\n<p>three <b> four </b> five</p>  "),
            ShowNotes {
                blocks: vec![
                    Block::Paragraph(vec![plain("one two")]),
                    Block::Paragraph(vec![
                        plain("three "),
                        Run {
                            text: "four ".to_owned(),
                            style: RunStyle {
                                bold: true,
                                ..RunStyle::default()
                            },
                            link: None,
                        },
                        plain("five"),
                    ]),
                ]
            }
        );
    }

    #[test]
    fn line_breaks_stay_inside_the_paragraph_and_trim_at_its_edges() {
        assert_eq!(
            parse("<br>first <br> second<br><br>third<br>"),
            ShowNotes {
                blocks: vec![Block::Paragraph(vec![plain("first\nsecond\n\nthird")])]
            }
        );
    }

    #[test]
    fn empty_notes_have_no_blocks() {
        assert_eq!(parse(""), ShowNotes::default());
        assert_eq!(parse("<p> </p><ul></ul>"), ShowNotes::default());
    }

    #[test]
    fn typical_episode() {
        insta::assert_snapshot!(outline(&parse(include_str!(
            "fixtures/typical_episode.html"
        ))));
    }

    #[test]
    fn plain_text_notes() {
        insta::assert_snapshot!(outline(&parse(include_str!(
            "fixtures/plain_text_notes.html"
        ))));
    }

    #[test]
    fn structured_notes() {
        insta::assert_snapshot!(outline(&parse(include_str!(
            "fixtures/structured_notes.html"
        ))));
    }

    #[test]
    fn tags_outside_the_allowlist_render_as_plain_text() {
        insta::assert_snapshot!(outline(&parse(include_str!(
            "fixtures/outside_the_allowlist.html"
        ))));
    }

    #[test]
    fn plain_text_flattens_the_blocks_into_one_line() {
        let notes =
            parse("<p>Two <b>engineers</b><br>take apart</p><ul><li>one</li><li>thing</li></ul>");
        assert_eq!(plain_text(&notes), "Two engineers take apart one thing");
        assert_eq!(plain_text(&parse("")), "");
    }
}
