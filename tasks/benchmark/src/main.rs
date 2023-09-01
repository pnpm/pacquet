use std::{fs, path::Path};

use criterion::{Criterion, Throughput};
use mockito::ServerGuard;
use pacquet_tarball::download_tarball_to_store;
use pico_args::Arguments;
use project_root::get_project_root;
use tempfile::tempdir;

fn bench_tarball(c: &mut Criterion, server: &mut ServerGuard, fixtures_folder: &Path) {
    let mut group = c.benchmark_group("tarball");
    let file = fs::read(fixtures_folder.join("@fastify+error-3.3.0.tgz")).unwrap();
    server.mock("GET", "/@fastify+error-3.3.0.tgz").with_status(201).with_body(&file).create();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();

    let url = &format!("{0}/@fastify+error-3.3.0.tgz", server.url());

    group.throughput(Throughput::Bytes(file.len() as u64));
    group.bench_function("download_dependency", |b| {
        b.to_async(&rt).iter(|| async {
            let dir = tempdir().unwrap();
            let cas_map =
                download_tarball_to_store(
                    &Default::default(),
                    dir.path().into(),
                    "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
                    Some(16697),
                    url,
                ).await.unwrap();
            drop(dir);
            cas_map.len()
        });
    });

    group.finish();
}

pub fn main() -> Result<(), String> {
    let mut server = mockito::Server::new();
    let mut args = Arguments::from_env();
    let root = get_project_root().unwrap();
    let fixtures_folder = root.join("tasks/benchmark/fixtures");
    let baseline: Option<String> = args.opt_value_from_str("--save-baseline").unwrap();

    let mut criterion = Criterion::default().without_plots();
    if let Some(ref baseline) = baseline {
        criterion = criterion.save_baseline(baseline.to_string());
    }

    bench_tarball(&mut criterion, &mut server, &fixtures_folder);

    Ok(())
}
