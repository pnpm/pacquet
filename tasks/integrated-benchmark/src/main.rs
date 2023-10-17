mod cli_args;
mod fixtures;
mod verdaccio;
mod verify;
mod work_env;

#[tokio::main]
async fn main() {
    let cli_args::CliArgs {
        scenario,
        registry,
        verdaccio,
        repository,
        package_json,
        hyperfine_options,
        work_env,
        with_pnpm,
        revisions,
    } = clap::Parser::parse();
    let repository = std::fs::canonicalize(repository).expect("get absolute path to repository");
    if !work_env.exists() {
        std::fs::create_dir_all(&work_env).expect("create work env");
    }
    let work_env = std::fs::canonicalize(work_env).expect("get absolute path to work env");
    let verdaccio = if verdaccio {
        verify::ensure_program("verdaccio");
        verdaccio::VerdaccioOptions {
            client: &Default::default(),
            listen: &registry,
            stdout: &work_env.join("verdaccio.stdout"),
            stderr: &work_env.join("verdaccio.stderr"),
            max_retries: 5,
            retry_delay: tokio::time::Duration::from_millis(500),
        }
        .spawn_if_necessary()
        .await
    } else {
        verify::ensure_virtual_registry(&registry).await;
        None
    };
    verify::ensure_git_repo(&repository);
    verify::validate_revision_list(&revisions);
    verify::ensure_program("bash");
    verify::ensure_program("cargo");
    verify::ensure_program("git");
    verify::ensure_program("hyperfine");
    verify::ensure_program("pnpm");
    work_env::WorkEnv {
        root: work_env,
        with_pnpm,
        revisions,
        registry,
        repository,
        scenario,
        hyperfine_options,
        package_json,
    }
    .run();
    drop(verdaccio); // terminate verdaccio if exists
}
