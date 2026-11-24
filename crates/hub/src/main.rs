fn main() -> std::process::ExitCode {
    // Before any other thread exists: a replacement started by `restart_hub` takes its marker out of
    // the environment here, so nothing this hub starts later inherits it.
    krypto_hub::restart::take_marker();
    match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt.block_on(krypto_hub::cli::run()),
        Err(e) => {
            eprintln!("error: could not start the async runtime: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
