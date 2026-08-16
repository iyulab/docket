//! Verifies a downloaded release asset against the `checksums.txt` the
//! release build also publishes (generated with `sha256sum * > checksums.txt`,
//! so the format here must match `sha256sum`'s own output: `<hex>  <name>`).

use sha2::{Digest, Sha256};

/// sha256 of `bytes`, lowercase hex — the same format `sha256sum` produces.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Looks up the expected hash for `asset_name` in a `checksums.txt` body
/// (lines of `<sha256>  <filename>`).
pub fn expected_hash<'a>(checksums_txt: &'a str, asset_name: &str) -> Option<&'a str> {
    checksums_txt.lines().find_map(|line| {
        let (hash, name) = line.split_once("  ")?;
        (name.trim() == asset_name).then_some(hash.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_known_content() {
        // printf 'hello\n' | sha256sum
        assert_eq!(
            sha256_hex(b"hello\n"),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn expected_hash_finds_matching_line() {
        let txt = "aaa111  docket-mcp-x86_64-pc-windows-msvc.exe\nbbb222  docket-mcp-x86_64-unknown-linux-gnu\n";
        assert_eq!(
            expected_hash(txt, "docket-mcp-x86_64-pc-windows-msvc.exe"),
            Some("aaa111")
        );
    }

    #[test]
    fn expected_hash_is_none_when_not_listed() {
        let txt = "aaa111  docket-mcp-x86_64-pc-windows-msvc.exe\n";
        assert_eq!(expected_hash(txt, "docket-mcp-aarch64-apple-darwin"), None);
    }
}
