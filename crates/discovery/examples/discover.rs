//! Print discovered agents as JSON.
//! `cargo run -p pawl-discovery --example discover -- [cache_dir] [--offline]`
use pawl_discovery::{discover, DiscoveryConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let offline = args.iter().any(|a| a == "--offline");
    let cache_dir = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(std::path::PathBuf::from)
        .or_else(pawl_protocol::data_dir)
        .ok_or_else(|| anyhow::anyhow!("no cache dir"))?;
    // The example has no hub identity; stamp a fixed placeholder machine id.
    let machine_id = "example00000".to_string();
    let agents = discover(&DiscoveryConfig { machine_id, cache_dir, custom_agents: vec![], offline }).await?;
    println!("{}", serde_json::to_string_pretty(&agents)?);
    Ok(())
}
