//! krypto-hub: the Krypto hub daemon (library half; the `krypto` binary is a thin CLI on top).
pub mod bus;
pub mod cli;
pub mod config;
pub mod core;
pub mod logging;
pub mod machine;
pub mod repos;
pub mod notify;
pub mod server;
pub mod store;

pub const HUB_VERSION: &str = env!("CARGO_PKG_VERSION");
