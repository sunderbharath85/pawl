//! pawl-hub: the Pawl hub daemon (library half; the `pawl` binary is a thin CLI on top).
pub mod bus;
pub mod cli;
pub mod config;
pub mod core;
pub mod logging;
pub mod machine;
pub mod repos;
pub mod restart;
pub mod notify;
pub mod server;
pub mod settings;
pub mod store;
pub mod titles;

pub const HUB_VERSION: &str = env!("CARGO_PKG_VERSION");
