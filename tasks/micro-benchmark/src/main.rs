use std::{fs, path::Path};

use clap::Parser;
use criterion::{Criterion, Throughput};
use mockito::ServerGuard;
use pacquet_tarball::download_tarball_to_store;
use pipe_trait::Pipe;
use project_root::get_project_root;
use reqwest::Client;
use tempfile::tempdir;

#[derive(Debug, Parser)]
struct CliArgs {
    #[clap(long)]
    save_baseline: Option<String>,
}

fn bench_tarball(c: &mut Criterion, server: &mut ServerGuard, fixtures_folder: &Path) {
    let mut group = c.benchmark_group("tarball");
    let file = fs::read(fixtures_folder.join("@fastify+error-3.3.0.tgz")).unwrap();
    server.mock("GET", "/@fastify+error-3.3.0.tgz").with_status(201).with_body(&file).create();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();

    let url = &format!("{0}/@fastify+error-3.3.0.tgz", server.url());

    group.throughput(Throughput::Bytes(file.len() as u64));
    group.bench_function("download_dependency", |b| {
        b.to_async(&rt).iter(|| async {
            // NOTE: the tempdir is being leaked, meaning the cleanup would be postponed until the end of the benchmark
            let dir = tempdir().unwrap().pipe(Box::new).pipe(Box::leak);
            let http_client = Client::new();

            let cas_map =
                download_tarball_to_store(
                    &Default::default(),
                    &http_client,
                    dir.path(),
                    "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
                    Some(16697),
                    url,
                ).await.unwrap();
            cas_map.len()
        });
    });

    group.finish();
}

pub fn main() -> Result<(), String> {
    let mut server = mockito::Server::new();
    let CliArgs { save_baseline } = CliArgs::parse();
    let root = get_project_root().unwrap();
    let fixtures_folder = root.join("tasks/micro-benchmark/fixtures");

    let mut criterion = Criterion::default().without_plots();
    if let Some(baseline) = save_baseline {
        criterion = criterion.save_baseline(baseline);
    }

    bench_tarball(&mut criterion, &mut server, &fixtures_folder);

    Ok(())
}