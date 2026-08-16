//! Maps this process's OS/arch onto the release asset naming convention
//! `.github/workflows/release.yml` produces (`<binary>-<arch>-<os-triple>[.exe]`).

/// The release asset name for a given `os`/`arch` pair (as `std::env::consts`
/// would report them) and binary name — e.g. `("windows", "x86_64",
/// "docket-mcp")` -> `"docket-mcp-x86_64-pc-windows-msvc.exe"`. Takes `os`/
/// `arch` as parameters (rather than reading `std::env::consts` directly) so
/// every platform combination is unit-testable regardless of which platform
/// the test suite actually runs on.
pub fn asset_name_for(os: &str, arch: &str, binary: &str) -> Option<String> {
    let arch = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    let (os, ext) = match os {
        "windows" => ("pc-windows-msvc", ".exe"),
        "macos" => ("apple-darwin", ""),
        "linux" => ("unknown-linux-gnu", ""),
        _ => return None,
    };
    Some(format!("{binary}-{arch}-{os}{ext}"))
}

/// `asset_name_for` using this process's actual OS/arch.
pub fn current_asset_name(binary: &str) -> Option<String> {
    asset_name_for(std::env::consts::OS, std::env::consts::ARCH, binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_x86_64() {
        assert_eq!(
            asset_name_for("windows", "x86_64", "docket-mcp").as_deref(),
            Some("docket-mcp-x86_64-pc-windows-msvc.exe")
        );
    }

    #[test]
    fn macos_aarch64() {
        assert_eq!(
            asset_name_for("macos", "aarch64", "docket-mcp").as_deref(),
            Some("docket-mcp-aarch64-apple-darwin")
        );
    }

    #[test]
    fn linux_x86_64() {
        assert_eq!(
            asset_name_for("linux", "x86_64", "docket-mcp").as_deref(),
            Some("docket-mcp-x86_64-unknown-linux-gnu")
        );
    }

    #[test]
    fn unsupported_os_is_none() {
        assert_eq!(asset_name_for("freebsd", "x86_64", "docket-mcp"), None);
    }

    #[test]
    fn unsupported_arch_is_none() {
        assert_eq!(asset_name_for("windows", "riscv64", "docket-mcp"), None);
    }

    #[test]
    fn current_platform_resolves_to_something() {
        // This test suite only ever runs on platforms the release build
        // actually targets, so this must always be Some(_).
        assert!(current_asset_name("docket-mcp").is_some());
    }
}
