//! Show notes to safe HTML (spec section 2.2).
//!
//! Every description leaves here as HTML, so clients never need to know
//! whether the publisher wrote plain text or markup.

use std::collections::HashSet;
use std::sync::LazyLock;

use ammonia::{Builder, UrlRelative};

/// The allowlist from spec 2.2. Images are dropped mostly because they're
/// tracking pixels. Relative links are dropped too: a client has no base URL
/// to resolve them against.
static CLEANER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut builder = Builder::empty();
    builder
        .add_tags([
            "p",
            "br",
            "a",
            "strong",
            "b",
            "em",
            "i",
            "u",
            "ul",
            "ol",
            "li",
            "blockquote",
            "code",
            "pre",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
        ])
        .add_tag_attributes("a", ["href"])
        .url_schemes(HashSet::from(["http", "https", "mailto"]))
        .url_relative(UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer nofollow"))
        // `Builder::empty` clears this set; without it the text inside
        // `<script>` and `<style>` would survive as visible text.
        .clean_content_tags(HashSet::from(["script", "style"]));
    builder
});

/// Turns a feed text field into sanitized HTML.
///
/// `declared_plain` is true when the feed marked the text as plain (Atom
/// `type="text"`). RSS never says, so otherwise [`looks_like_html`] decides.
/// Plain text is escaped and keeps its line breaks as `<br>`.
pub(super) fn to_html(text: &str, declared_plain: bool) -> String {
    let text = text.trim();
    if declared_plain || !looks_like_html(text) {
        return escape_plain(text);
    }
    let html = CLEANER.clean(text).to_string();
    let html = html.trim();
    // Many hosts write plain-text notes with the odd link or entity in them
    // and rely on newlines for layout. Without any line-level tag, those
    // newlines are the only structure, so they become `<br>`.
    if has_line_structure(html) {
        html.to_owned()
    } else {
        html.replace("\r\n", "\n").replace('\n', "<br>")
    }
}

/// Whether sanitized HTML lays out its own lines. The cleaner strips every
/// attribute from these tags, so matching the bare tag is exact.
fn has_line_structure(html: &str) -> bool {
    [
        "<p>",
        "<br>",
        "<li>",
        "<pre>",
        "<blockquote>",
        "<h1>",
        "<h2>",
        "<h3>",
        "<h4>",
        "<h5>",
        "<h6>",
    ]
    .iter()
    .any(|tag| html.contains(tag))
}

/// True if `text` contains something an HTML parser would treat as markup: a
/// tag-like `<x`, `</`, `<!`, or a character reference (`&amp;`, `&#8217;`,
/// `&#x2019;`).
///
/// A stray `<` or `&` in prose (`a < b`, `Q&A`) doesn't count, so such text
/// stays plain and keeps its line breaks.
fn looks_like_html(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(index, &byte)| {
        let rest = &bytes[index + 1..];
        match byte {
            b'<' => rest
                .first()
                .is_some_and(|next| next.is_ascii_alphabetic() || matches!(next, b'/' | b'!')),
            b'&' => is_character_reference(rest),
            _ => false,
        }
    })
}

/// Whether `rest` (the bytes after a `&`) starts with `name;`, `#digits;` or
/// `#xhex;`.
fn is_character_reference(rest: &[u8]) -> bool {
    let (body, valid): (&[u8], fn(&u8) -> bool) = match rest {
        [b'#', b'x' | b'X', body @ ..] => (body, u8::is_ascii_hexdigit),
        [b'#', body @ ..] => (body, u8::is_ascii_digit),
        [first, ..] if first.is_ascii_alphabetic() => (rest, u8::is_ascii_alphanumeric),
        _ => return false,
    };
    let len = body.iter().take_while(|byte| valid(byte)).count();
    len > 0 && body.get(len) == Some(&b';')
}

fn escape_plain(text: &str) -> String {
    let mut html = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '\r' => {
                chars.next_if_eq(&'\n');
                html.push_str("<br>");
            }
            '\n' => html.push_str("<br>"),
            ch => html.push(ch),
        }
    }
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_escaped_and_keeps_line_breaks() {
        assert_eq!(
            to_html("  Q&A: a < b\r\nnext \"line\"\n", false),
            "Q&amp;A: a &lt; b<br>next \"line\""
        );
    }

    #[test]
    fn declared_plain_text_is_escaped_even_if_it_looks_like_html() {
        assert_eq!(to_html("<b>hi</b>", true), "&lt;b&gt;hi&lt;/b&gt;");
    }

    #[test]
    fn entities_alone_mark_text_as_html() {
        assert_eq!(
            to_html("Tom &amp; Jerry&#8217;s", false),
            "Tom &amp; Jerry’s"
        );
    }

    #[test]
    fn html_keeps_only_the_allowlist() {
        let dirty = concat!(
            r#"<p onclick="x()" class="c">Hi <img src="https://t.example/p.gif">"#,
            r#"<a href="https://example.com" target="_blank" onmouseover="x()">link</a>"#,
            r#"<a href="javascript:alert(1)">js</a><a href="/relative">rel</a>"#,
            r#"<a href="mailto:host@example.com">mail</a></p>"#,
            r#"<script>alert(1)</script><style>p{}</style>"#,
            r#"<iframe src="https://example.com"></iframe><form><input></form>"#,
            r#"<div><h2>Title</h2><ul><li><strong>x</strong></li></ul></div>"#,
        );
        assert_eq!(
            to_html(dirty, false),
            concat!(
                r#"<p>Hi <a href="https://example.com" rel="noopener noreferrer nofollow">link</a>"#,
                r#"<a rel="noopener noreferrer nofollow">js</a><a rel="noopener noreferrer nofollow">rel</a>"#,
                r#"<a href="mailto:host@example.com" rel="noopener noreferrer nofollow">mail</a></p>"#,
                r#"<h2>Title</h2><ul><li><strong>x</strong></li></ul>"#,
            )
        );
    }

    #[test]
    fn html_without_line_tags_keeps_its_newlines() {
        assert_eq!(
            to_html(
                "Intro\n\nSponsor: <a href=\"https://e.com/?a=1&amp;b=2\">deal</a>\nBye",
                false
            ),
            concat!(
                "Intro<br><br>Sponsor: ",
                r#"<a href="https://e.com/?a=1&amp;b=2" rel="noopener noreferrer nofollow">deal</a>"#,
                "<br>Bye"
            )
        );
        assert_eq!(
            to_html("<p>One</p>\n<p>Two</p>", false),
            "<p>One</p>\n<p>Two</p>"
        );
    }

    #[test]
    fn character_reference_detection() {
        assert!(looks_like_html("a &#x2019; b"));
        assert!(looks_like_html("x<br/>y"));
        assert!(looks_like_html("<!-- c -->"));
        assert!(!looks_like_html("AT&T & friends; 3 < 4 &#; &x"));
    }
}
