//! Minimal server-sent-events parser and the shared stream pump.
//!
//! The adapters need only the `data:` payloads. Event names are not
//! parsed; the payload's own `type` field is authoritative. The parser
//! accepts LF and CRLF framing and arbitrary chunk boundaries.
//!
//! [`spawn_event_stream`] pumps one HTTP response through a
//! [`PayloadMapper`] into an [`EventStream`]. Each adapter supplies the
//! mapper; the reader task, abort handling, and channel plumbing stay
//! in one place.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use crate::{EventStream, ProviderError, StreamEvent};

/// Events per wire type, for the debug trace. Counted only when the
/// trace runs at `debug`; below that the map stays empty and every
/// call is one level check.
#[derive(Default)]
pub(crate) struct EventCounts(BTreeMap<String, u32>);

impl EventCounts {
    /// Count one wire event of type `kind`.
    pub(crate) fn count(&mut self, kind: &str) {
        if !log::log_enabled!(log::Level::Debug) {
            return;
        }
        match self.0.get_mut(kind) {
            Some(count) => *count += 1,
            None => {
                self.0.insert(kind.to_string(), 1);
            }
        }
    }

    /// The per-type counts, one line, at the stream's end.
    pub(crate) fn report(&self) {
        if log::log_enabled!(log::Level::Debug) {
            let counts: Vec<String> = self
                .0
                .iter()
                .map(|(kind, count)| format!("{kind}={count}"))
                .collect();
            log::debug!("sse events {}", counts.join(" "));
        }
    }
}

/// Adapter-specific mapping from SSE payloads to neutral events.
pub(crate) trait PayloadMapper: Send + 'static {
    /// Map one `data:` payload. Unknown payloads map to nothing.
    fn map(&mut self, payload: &str) -> Vec<Result<StreamEvent, ProviderError>>;

    /// True once the wire protocol said the response is complete.
    fn terminal(&self) -> bool;

    /// The error for a stream that ends before the terminal event.
    fn eof_error(&self) -> ProviderError;
}

struct ChannelStream(tokio::sync::mpsc::Receiver<Result<StreamEvent, ProviderError>>);

impl Stream for ChannelStream {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// Read the response body on a task and map it into an event stream.
pub(crate) fn spawn_event_stream(
    mut response: reqwest::Response,
    mut mapper: impl PayloadMapper,
) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        let mut parser = SseParser::new();
        loop {
            // A dropped receiver means the turn was aborted: stop
            // reading and release the connection instead of waiting
            // for the next chunk that may never come.
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk,
                () = tx.closed() => return,
            };
            match chunk {
                Ok(Some(bytes)) => {
                    for payload in parser.push(&bytes) {
                        log::trace!("sse payload {payload}");
                        for event in mapper.map(&payload) {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                        if mapper.terminal() {
                            return;
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    log::warn!("stream failed: network error: {error}");
                    let _ = tx
                        .send(Err(ProviderError::Network(error.to_string())))
                        .await;
                    return;
                }
            }
        }
        if !mapper.terminal() {
            log::warn!("stream ended before the terminal event");
            let _ = tx.send(Err(mapper.eof_error())).await;
        }
    });
    Box::pin(ChannelStream(rx))
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    /// No complete record delimiter ends before this index. The next
    /// scan starts here, so total parsing cost stays linear in bytes
    /// received instead of rescanning the buffer per record.
    scan_from: usize,
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
        let mut start = 0;
        let mut scan = self.scan_from;
        while let Some((record_end, delim_len)) = find_record_end(&self.buf[start..], scan - start)
        {
            let record_end = start + record_end;
            if let Some(data) = parse_record(&self.buf[start..record_end]) {
                out.push(data);
            }
            start = record_end + delim_len;
            scan = start;
        }
        // Keep the unconsumed tail in one drain. The next scan backs
        // off three bytes: a delimiter can straddle the chunk
        // boundary, and its longest form is four bytes.
        self.buf.drain(..start);
        self.scan_from = self.buf.len().saturating_sub(3);
        out
    }
}

/// Find the earliest record delimiter: `\n\n` or `\r\n\r\n`. One pass
/// over the bytes: each newline checks both patterns ending on it in
/// constant time. The caller guarantees no delimiter starts before
/// `from`, so the search skips that prefix.
fn find_record_end(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    for at in from.max(1)..buf.len() {
        if buf[at] != b'\n' {
            continue;
        }
        if buf[at - 1] == b'\n' {
            return Some((at - 1, 2));
        }
        if at >= 3 && &buf[at - 3..=at] == b"\r\n\r\n" {
            return Some((at - 3, 4));
        }
    }
    None
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

    #[test]
    fn a_delimiter_split_across_chunks_still_ends_the_record() {
        // Every split point of the four-byte CRLF delimiter.
        for split in 1..4 {
            let full = b"data: x\r\n\r\ndata: y\r\n\r\n".to_vec();
            let cut = 7 + split;
            let mut parser = SseParser::new();
            assert!(parser.push(&full[..cut]).is_empty(), "split {split}");
            assert_eq!(parser.push(&full[cut..]), vec!["x", "y"], "split {split}");
        }
    }

    /// The old exhaustive-search parser, kept as the reference: scan
    /// the whole buffer for both delimiters and take the earlier one.
    fn reference_parse(bytes: &[u8]) -> Vec<String> {
        fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        }
        let mut buf = bytes.to_vec();
        let mut out = Vec::new();
        loop {
            let lf = find(&buf, b"\n\n");
            let crlf = find(&buf, b"\r\n\r\n");
            let (end, delim) = match (lf, crlf) {
                (Some(a), Some(b)) if b < a => (b, 4),
                (Some(a), _) => (a, 2),
                (None, Some(b)) => (b, 4),
                (None, None) => break,
            };
            if let Some(data) = parse_record(&buf[..end]) {
                out.push(data);
            }
            buf.drain(..end + delim);
        }
        out
    }

    #[test]
    fn matches_the_reference_parser_on_adversarial_streams() {
        // A deterministic LCG builds byte streams heavy in CR and LF
        // runs; every stream runs whole, halved, and byte-at-a-time.
        let fragments: &[&[u8]] = &[
            b"data: a",
            b"\n",
            b"\r",
            b"\r\n",
            b"\n\n",
            b"\r\n\r\n",
            b": note",
            b"data: b\n\ndata: c",
            b"x",
        ];
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        for _ in 0..300 {
            let mut stream = Vec::new();
            for _ in 0..24 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let pick = (seed >> 33) as usize % fragments.len();
                stream.extend_from_slice(fragments[pick]);
            }
            let expected = reference_parse(&stream);

            let mut whole = SseParser::new();
            assert_eq!(whole.push(&stream), expected);

            let mut halves = SseParser::new();
            let mid = stream.len() / 2;
            let mut got = halves.push(&stream[..mid]);
            got.extend(halves.push(&stream[mid..]));
            assert_eq!(got, expected);

            let mut bytewise = SseParser::new();
            let mut got = Vec::new();
            for byte in &stream {
                got.extend(bytewise.push(std::slice::from_ref(byte)));
            }
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn many_records_in_one_chunk_parse_in_order() {
        let mut chunk = Vec::new();
        for index in 0..500 {
            chunk.extend_from_slice(format!("data: {index}\n\n").as_bytes());
        }
        let mut parser = SseParser::new();
        let out = parser.push(&chunk);
        assert_eq!(out.len(), 500);
        assert_eq!(out[0], "0");
        assert_eq!(out[499], "499");
    }
}
