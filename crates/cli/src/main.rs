use anyhow::Result;

pub fn main() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(pacquet_cli::run_commands())
}
