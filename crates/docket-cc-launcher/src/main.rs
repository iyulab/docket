//! `docket-cc` launcher: installed as `docket-cc`, this binary resolves the
//! latest `docket-cc` worker build via `docket_launcher_core::resolve_and_run`
//! (GitHub Releases check, cache, checksum verify) and execs it with this
//! process's stdio inherited, forwarding every argument it was called with —
//! including the `hook` subcommand a Claude Code `SessionStart` hook invokes
//! (see `docket-cc`'s own `main.rs` for what `hook` does).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = collect_forwarded_args(std::env::args());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = docket_launcher_core::resolve_and_run("docket-cc", &arg_refs).await?;
    std::process::exit(code);
}

/// Everything after argv[0] — e.g. `docket-cc-launcher hook` on the command
/// line becomes `["hook"]`, which `resolve_and_run` forwards unchanged to
/// the resolved `docket-cc` binary so its `hook` subcommand still fires when
/// invoked through this launcher.
fn collect_forwarded_args(argv: impl Iterator<Item = String>) -> Vec<String> {
    argv.skip(1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_subcommand_argument_is_forwarded() {
        let argv = vec!["docket-cc-launcher".to_string(), "hook".to_string()];
        assert_eq!(collect_forwarded_args(argv.into_iter()), vec!["hook"]);
    }

    #[test]
    fn no_arguments_forwards_an_empty_slice() {
        let argv = vec!["docket-cc-launcher".to_string()];
        assert_eq!(
            collect_forwarded_args(argv.into_iter()),
            Vec::<String>::new()
        );
    }
}
