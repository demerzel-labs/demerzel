//! Snapshot tags for the read, edit, and create tools.
//!
//! A tag is the FNV-1a 64-bit hash of the whole file content, xor-folded
//! to 24 bits and printed as six hex characters. The read tool stamps
//! every output with the tag; the edit tool accepts line numbers only
//! while the live file still hashes to the same tag. One mismatch means
//! the file changed and every line number from that read is void.
//!
//! Line numbers are unique by construction, so no edit target is ever
//! ambiguous: file drift collapses into one precise stale-tag error
//! with one recovery — read again.
//!
//! Header parse rule, for every consumer of tool output: the header is
//! exactly line 1, shaped `[path#tag]`. The tag is the six characters
//! before the final `]`; the path is everything between the leading
//! `[` and the last `#`, and may itself contain `#` or `]`.

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Tag one file snapshot. `text` is the exact file content; the tag
/// changes when any byte changes.
pub fn file_tag(text: &str) -> String {
    let mut hash = FNV_OFFSET;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let folded = (hash >> 32) ^ hash;
    format!("{:06x}", folded & 0x00ff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_six_lowercase_hex_characters() {
        for text in ["", "fn main() {}\n", "日本語\n"] {
            let tag = file_tag(text);
            assert_eq!(tag.len(), 6);
            assert!(
                tag.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }
    }

    #[test]
    fn tags_depend_on_the_exact_bytes() {
        assert_eq!(file_tag("one\ntwo\n"), file_tag("one\ntwo\n"));
        assert_ne!(file_tag("one\ntwo\n"), file_tag("one\ntwo"));
        assert_ne!(file_tag("one\ntwo\n"), file_tag("one\r\ntwo\r\n"));
    }

    #[test]
    fn tag_values_are_pinned() {
        // The edit tool validates against these exact values and the
        // session record replays them. Changing the hash is a breaking
        // change.
        assert_eq!(file_tag(""), "d0bfc1");
        assert_eq!(file_tag("hello\nworld\n"), "1d0a3d");
        assert_eq!(file_tag("one\ntwo\nthree\n"), "7b258d");
    }
}
