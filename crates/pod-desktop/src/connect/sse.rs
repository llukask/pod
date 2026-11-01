//! The part of server-sent events the Connect stream uses (spec 14.2): the
//! `data:` lines of each event, with comments and other fields skipped.
//!
//! Bytes arrive in whatever chunks the connection delivers, so a line, or
//! a multi-byte character, can be split anywhere. The parser keeps the
//! unfinished line and hands back an event's data once the blank line that
//! ends it arrives.

/// Turns a byte stream into the data of its events.
#[derive(Debug, Default)]
pub struct Parser {
    /// The current line, until its end arrives.
    line: Vec<u8>,
    /// The current event's `data:` lines, joined by newlines.
    data: Option<String>,
    /// The last byte was a `\r`, so a `\n` right after it ends nothing.
    after_cr: bool,
}

impl Parser {
    /// Feeds `chunk` and returns the data of every event it completed,
    /// oldest first. An event without `data:` lines yields nothing.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut events = Vec::new();
        for &byte in chunk {
            match byte {
                b'\n' if self.after_cr => self.after_cr = false,
                b'\n' | b'\r' => {
                    self.after_cr = byte == b'\r';
                    if let Some(data) = self.end_line() {
                        events.push(data);
                    }
                }
                _ => {
                    self.after_cr = false;
                    self.line.push(byte);
                }
            }
        }
        events
    }

    /// Handles the finished line. A blank one ends the event.
    fn end_line(&mut self) -> Option<String> {
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            return self.data.take();
        }
        // A comment, such as the server's `: ping`.
        if line.starts_with(b":") {
            return None;
        }
        let line = String::from_utf8_lossy(&line);
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_ref(), ""),
        };
        if field == "data" {
            match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_event_yields_its_data() {
        let mut parser = Parser::default();
        assert_eq!(
            parser.push(b"data: {\"type\":\"replaced\"}\n\ndata:{\"a\":1}\n\n"),
            [r#"{"type":"replaced"}"#, r#"{"a":1}"#]
        );
    }

    #[test]
    fn comments_and_other_fields_are_skipped() {
        let mut parser = Parser::default();
        assert_eq!(
            parser.push(b": ping\n\nevent: state\nid: 7\nretry: 1000\ndata: x\n\n"),
            ["x"]
        );
        assert!(parser.push(b": ping\n\n").is_empty(), "a ping is no event");
    }

    #[test]
    fn events_split_across_chunks_come_out_whole() {
        let event = "data: {\"title\":\"Küche\"}\n\n".as_bytes();
        // Every split point, the one inside the two-byte "ü" included.
        for split in 0..event.len() {
            let mut parser = Parser::default();
            let mut events = parser.push(&event[..split]);
            events.extend(parser.push(&event[split..]));
            assert_eq!(events, [r#"{"title":"Küche"}"#], "split at {split}");
        }
    }

    #[test]
    fn any_line_ending_counts() {
        let mut parser = Parser::default();
        assert_eq!(
            parser.push(b"data: a\r\n\r\ndata: b\r\rdata: c\n"),
            ["a", "b"]
        );
        assert_eq!(parser.push(b"\n"), ["c"]);
        let mut parser = Parser::default();
        assert_eq!(parser.push(b"data: d\r"), Vec::<String>::new());
        assert_eq!(
            parser.push(b"\n\r\n"),
            ["d"],
            "a CRLF split between chunks is one line end"
        );
    }

    #[test]
    fn data_lines_of_one_event_join_with_newlines() {
        let mut parser = Parser::default();
        assert_eq!(
            parser.push(b"data: one\ndata\ndata: two\n\n"),
            ["one\n\ntwo"]
        );
    }
}
