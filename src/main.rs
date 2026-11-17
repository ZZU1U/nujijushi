#[tokio::main]
async fn main() -> std::process::ExitCode {
    match nujijushi::cli::run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
