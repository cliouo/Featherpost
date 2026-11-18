#[tokio::main]
async fn main() -> std::process::ExitCode {
    match nodeseek_email::provider::run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nodeseek-email-provider: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
