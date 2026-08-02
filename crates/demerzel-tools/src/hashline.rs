//! Line tags for the read tool and the step-4 edit tool.
//!
//! A tag is the FNV-1a 64-bit hash of the line content, xor-folded to
//! 24 bits and printed as six hex characters. The tag depends on the
//! content only, so it is stable across positions, offsets, and reads.
//! Identical lines share one tag; the edit tool must reject an anchor
//! that matches more than one line. Prior art: oh-my-pi's Hashline.

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Tag one line of content. `line` carries no terminator; `\n` and
/// `\r\n` endings must be stripped before hashing.
pub fn line_tag(line: &str) -> String {
    let mut hash = FNV_OFFSET;
    for byte in line.bytes() {
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
        for line in ["", "fn main() {", "    return 0;", "日本語"] {
            let tag = line_tag(line);
            assert_eq!(tag.len(), 6);
            assert!(
                tag.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }
    }

    #[test]
    fn tags_depend_on_content_only() {
        // Stability across calls stands in for stability across reads,
        // files, and positions: the input is the only state.
        assert_eq!(line_tag("alpha"), line_tag("alpha"));
        assert_ne!(line_tag("alpha"), line_tag("beta"));
        assert_ne!(line_tag("alpha"), line_tag("alpha "));
    }

    #[test]
    fn tag_values_are_pinned() {
        // Session files and step-4 edit anchors depend on these exact
        // values. Changing the hash is a breaking change.
        assert_eq!(line_tag(""), "d0bfc1");
        assert_eq!(line_tag("fn main() {"), "74ebed");
        assert_eq!(line_tag("line text"), "e7edf3");
        assert_eq!(line_tag("    return 0;"), "25afdc");
    }
}
