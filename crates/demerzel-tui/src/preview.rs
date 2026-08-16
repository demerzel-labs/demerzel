//! Capped tool-input preview for approval prompts.

use serde_json::Value;

/// Byte cap for the tool input shown in an approval prompt.
const PREVIEW_BYTES: usize = 1200;

/// Serializes into a capped buffer, so a preview of a large input
/// never materializes the whole document.
struct CappedWriter {
    buf: Vec<u8>,
    cap: usize,
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() >= self.cap {
            return Err(std::io::Error::other("preview cap reached"));
        }
        let take = data.len().min(self.cap - self.buf.len());
        self.buf.extend_from_slice(&data[..take]);
        if take < data.len() {
            return Err(std::io::Error::other("preview cap reached"));
        }
        Ok(take)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Pretty tool input, capped for the screen.
pub fn input_preview(input: &Value) -> String {
    let mut writer = CappedWriter {
        buf: Vec::with_capacity(PREVIEW_BYTES.min(256)),
        cap: PREVIEW_BYTES,
    };
    let complete = serde_json::to_writer_pretty(&mut writer, input).is_ok();
    let text = String::from_utf8_lossy(&writer.buf).into_owned();
    if complete {
        text
    } else {
        format!("{text}\n[input truncated]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_preview_caps_long_inputs_without_serializing_them() {
        let short = json!({"path": "f.txt"});
        assert_eq!(
            input_preview(&short),
            serde_json::to_string_pretty(&short).unwrap()
        );

        let long = json!({"content": "é".repeat(200_000)});
        let preview = input_preview(&long);
        assert!(preview.len() < PREVIEW_BYTES + 100);
        assert!(preview.ends_with("[input truncated]"));
    }
}
