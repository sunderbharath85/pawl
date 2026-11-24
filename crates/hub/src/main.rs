#[tokio::main]
async fn main() -> std::process::ExitCode {
    krypto_hub::cli::run().await
}
