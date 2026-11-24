//! Dry-run (or apply) hook installation against a chosen home directory.
//! `cargo run -p krypto-discovery --example install_hooks -- <claude|codex|copilot-cli|copilot-vscode> <hub_exe> <home_dir> [--apply]`
//! Pass a scratch directory as `home_dir` to try it without touching your real configs.
use krypto_discovery::hooks::{install_with_home, HookTarget, InstallEnv};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() >= 3, "usage: install_hooks <target> <hub_exe> <home_dir> [--apply]");
    let home = PathBuf::from(&args[2]);
    let target = HookTarget::from_cli(&args[0], home.join("repo")).ok_or_else(|| anyhow::anyhow!("unknown target {}", args[0]))?;
    if let HookTarget::CopilotVscode { repo } = &target {
        std::fs::create_dir_all(repo)?;
    }
    let apply = args.iter().any(|a| a == "--apply");
    // Env overrides deliberately ignored: only `home_dir` is touched.
    print!("{}", install_with_home(target, &PathBuf::from(&args[1]), apply, &home, &InstallEnv::default())?);
    Ok(())
}
