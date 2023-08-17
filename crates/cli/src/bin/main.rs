use pacquet_diagnostics::Result;

#[tokio::main(flavor = "multi_thread")]
pub async fn main() -> Result<()> {
    pacquet_cli::run_cli().await
}
