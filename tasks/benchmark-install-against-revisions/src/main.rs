mod cli_args;
mod fixtures;
mod verify;
mod work_env;

#[tokio::main]
async fn main() {
    let cli_args::CliArgs { registry, repository, package_json, work_env, revisions } =
        clap::Parser::parse();
    let repository = std::fs::canonicalize(repository).expect("get absolute path to repository");
    if !work_env.exists() {
        std::fs::create_dir_all(&work_env).expect("create work env");
    }
    let work_env = std::fs::canonicalize(work_env).expect("get absolute path to work env");
    verify::ensure_virtual_registry(&registry).await;
    verify::ensure_git_repo(&repository);
    work_env::WorkEnv { root: work_env, revisions, registry, repository, package_json }.run()
}
