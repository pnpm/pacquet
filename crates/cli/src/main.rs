use anyhow::Result;

#[tokio::main(flavor = "current_thread")]
pub async fn main() -> Result<()> {
    pacquet_cli::run_commands().await
}
