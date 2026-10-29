import { expect, test } from "vitest";
import { showNotes } from "./showNotes";

function html(source: string): string {
  const host = document.createElement("div");
  host.append(showNotes(source));
  return host.innerHTML;
}

test("keeps the allowlisted structure", () => {
  expect(html("<h2>Links</h2><p>A <strong>b</strong> <em>c</em></p><ul><li>one</li></ul><pre><code>x</code></pre>")).toBe(
    "<h2>Links</h2><p>A <strong>b</strong> <em>c</em></p><ul><li>one</li></ul><pre><code>x</code></pre>",
  );
});

test("links keep only an absolute http(s) or mailto href and open in a new tab", () => {
  expect(html('<a href="https://example.com/a" onclick="x()" style="color:red">site</a>')).toBe(
    '<a href="https://example.com/a" target="_blank" rel="noopener noreferrer nofollow">site</a>',
  );
  expect(html('<a href="mailto:hi@example.com">mail</a>')).toContain('href="mailto:hi@example.com"');
  expect(html('<a href="javascript:alert(1)">x</a>')).toBe("<a>x</a>");
  expect(html('<a href="/relative">x</a>')).toBe("<a>x</a>");
});

test("drops scripts, styles and embeds with their contents", () => {
  expect(html('<p>ok</p><script>alert(1)</script><style>p{}</style><iframe src="https://x"></iframe><img src="https://t/p.gif">')).toBe(
    "<p>ok</p>",
  );
});

test("unwraps other elements and keeps their text", () => {
  expect(html('<div class="x"><span style="color:red">Sponsor:</span> <font>example.com</font></div>')).toBe(
    "Sponsor: example.com",
  );
});

test("drops every attribute but href", () => {
  expect(html('<p class="a" id="b" onmouseover="c()">text</p>')).toBe("<p>text</p>");
});

test("plain text keeps its paragraphs and line breaks", () => {
  expect(html("First line\nsecond line\n\nNext paragraph")).toBe("<p>First line<br>second line</p><p>Next paragraph</p>");
});
