//! Minimal server-sent-events parser.
//!
//! The adapter needs only the `data:` payloads. Event names are not
//! parsed; the payload's own `type` field is authoritative. The parser
//! accepts LF and CRLF framing and arbitrary chunk boundaries.

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk of bytes. Returns the complete `data` payloads
    /// that ended inside this chunk.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((record_end, delim_len)) = find_record_end(&self.buf) {
            let record: Vec<u8> = self.buf.drain(..record_end + delim_len).collect();
            if let Some(data) = parse_record(&record[..record_end]) {
                out.push(data);
            }
        }
        out
    }
}

/// Find the earliest record delimiter: `\n\n` or `\r\n\r\n`.
fn find_record_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = find_subslice(buf, b"\n\n");
    let crlf = find_subslice(buf, b"\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) if b < a => Some((b, 4)),
        (Some(a), _) => Some((a, 2)),
        (None, Some(b)) => Some((b, 4)),
        (None, None) => None,
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Join the record's `data:` lines with `\n`, per the SSE spec.
fn parse_record(record: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(record);
    let mut data_lines = Vec::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_record_split_across_chunks() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"data: {\"a\":").is_empty());
        let out = parser.push(b"1}\n\ndata: {\"b\":2}\n\n");
        assert_eq!(out, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    #[test]
    fn accepts_crlf_framing_and_event_lines() {
        let mut parser = SseParser::new();
        let out = parser.push(b"event: response.created\r\ndata: {\"x\":1}\r\n\r\n");
        assert_eq!(out, vec![r#"{"x":1}"#]);
    }

    #[test]
    fn joins_multi_line_data() {
        let mut parser = SseParser::new();
        let out = parser.push(b"data: line1\ndata: line2\n\n");
        assert_eq!(out, vec!["line1\nline2"]);
    }

    #[test]
    fn ignores_records_without_data() {
        let mut parser = SseParser::new();
        assert!(parser.push(b": keep-alive\n\n").is_empty());
    }
}
