#[tokio::main]
async fn main() -> std::process::ExitCode {
    pet_hub::cli::run().await
}
