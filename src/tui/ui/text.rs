use unicode_width::UnicodeWidthStr;

/// Truncate a string to fit within `max_width` display columns, appending
/// "…" if truncated. Correctly handles multi-byte UTF-8 and wide characters.
pub fn truncate(s: &str, max_width: usize) -> String {
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return s.to_string();
    }

    // We need to fit the content plus "…" (1 column) within max_width.
    let target = max_width.saturating_sub(1);
    let mut current_width = 0;
    let mut end = 0;

    for (i, ch) in s.char_indices() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > target {
            break;
        }
        current_width += ch_width;
        end = i + ch.len_utf8();
    }

    format!("{}…", &s[..end])
}

/// Pad a string with spaces to exactly `width` display columns. Truncates
/// first if necessary.
pub fn pad(s: &str, width: usize) -> String {
    let truncated = truncate(s, width);
    let current = UnicodeWidthStr::width(truncated.as_str());
    if current >= width {
        truncated
    } else {
        format!("{}{}", truncated, " ".repeat(width - current))
    }
}

/// Render a string that auto-scrolls horizontally when it exceeds
/// `max_width`. The `tick` counter drives the scroll position; it pauses
/// briefly at the start before scrolling, then pauses at the end before
/// wrapping around. Returns a string of exactly `max_width` display columns.
pub fn scroll(s: &str, max_width: usize, tick: usize) -> String {
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return pad(s, max_width);
    }

    // Collect chars with their display widths for safe slicing.
    let chars: Vec<(usize, char)> = s
        .chars()
        .map(|ch| (unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0), ch))
        .collect();

    // Scroll 2 columns per tick for a snappier feel.
    // Pause briefly (3 ticks ≈ 750ms) at each end before reversing.
    let speed = 2;
    let pause = 3;
    let overflow = width - max_width;
    let steps = (overflow + speed - 1) / speed; // ceil division
    let cycle_len = pause + steps + pause;
    let pos_in_cycle = tick % cycle_len;

    let char_offset = if pos_in_cycle < pause {
        0
    } else if pos_in_cycle < pause + steps {
        ((pos_in_cycle - pause) * speed).min(overflow)
    } else {
        overflow
    };

    // Skip `char_offset` display columns worth of characters.
    let mut skipped = 0;
    let mut start_idx = 0;
    for (i, (w, _)) in chars.iter().enumerate() {
        if skipped >= char_offset {
            start_idx = i;
            break;
        }
        skipped += w;
        start_idx = i + 1;
    }

    // Take characters that fit in max_width.
    let mut taken_width = 0;
    let mut result = String::new();
    for &(w, ch) in &chars[start_idx..] {
        if taken_width + w > max_width {
            break;
        }
        result.push(ch);
        taken_width += w;
    }

    // Pad if we're at the end and have leftover space.
    while taken_width < max_width {
        result.push(' ');
        taken_width += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_no_truncation() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn ascii_truncation() {
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn unicode_truncation() {
        // "über" — the ü is 2 bytes but 1 display column.
        let result = truncate("über den neuen Faschismus", 10);
        assert!(!result.is_empty());
        assert!(UnicodeWidthStr::width(result.as_str()) <= 10);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn pad_short_string() {
        let result = pad("hi", 5);
        assert_eq!(result, "hi   ");
        assert_eq!(UnicodeWidthStr::width(result.as_str()), 5);
    }

    #[test]
    fn pad_unicode() {
        let result = pad("über", 8);
        assert_eq!(UnicodeWidthStr::width(result.as_str()), 8);
    }
}
