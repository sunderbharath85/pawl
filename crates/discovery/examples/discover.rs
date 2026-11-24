//! Print discovered agents as JSON.
//! `cargo run -p pet-discovery --example discover -- [cache_dir] [--offline]`
use pet_discovery::{discover, DiscoveryConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let offline = args.iter().any(|a| a == "--offline");
    let cache_dir = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(std::path::PathBuf::from)
        .or_else(pet_protocol::data_dir)
        .ok_or_else(|| anyhow::anyhow!("no cache dir"))?;
    let agents = discover(&DiscoveryConfig { cache_dir, custom_agents: vec![], offline }).await?;
    println!("{}", serde_json::to_string_pretty(&agents)?);
    Ok(())
}
