use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};
use spindex::core::{db, scanner};

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let folder = match args.next() {
        Some(f) => PathBuf::from(f),
        None => bail!("usage: spindex-scan <folder> [db-file]\n       default db-file: ./spindex.db"),
    };
    let db_path = PathBuf::from(args.next().unwrap_or_else(|| "spindex.db".to_string()));

    if !folder.is_dir() {
        bail!("not a folder: {}", folder.display());
    }

    let mut conn = db::open(&db_path)?;
    println!("scanning {} into {}", folder.display(), db_path.display());

    let summary = scanner::scan_folder(&mut conn, &folder)?;

    println!();
    println!("files found     {:>8}", summary.found);
    println!("new             {:>8}", summary.new);
    println!("updated         {:>8}", summary.updated);
    println!("unchanged       {:>8}", summary.unchanged);
    println!("errors (skipped){:>8}", summary.errors);
    println!("elapsed         {:>8.2} s", summary.elapsed_secs);
    println!("tracks/second   {:>8.0}", summary.tracks_per_second());
    Ok(())
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
