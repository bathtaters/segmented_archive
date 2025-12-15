pub(crate) mod rolling_writer;
pub(crate) mod logger;
pub(crate) mod helpers;

use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::path::{PathBuf};
use std::fs;
use std::env;
use log::{info, error, LevelFilter};
use crate::logger::{init_logger, set_log_path};
use crate::helpers::{create_archive};

// --- Structs ---

const CONFIG_PATH: &str = "config.toml"; // Default
const LOG_LEVEL: LevelFilter = LevelFilter::Info;

#[derive(Debug, serde::Deserialize)]
struct Config {
    output_path: Option<PathBuf>,
    root_path: Option<PathBuf>,
    post_script: Option<PathBuf>,
    log_file: Option<PathBuf>,
    compression_level: Option<u32>,
    max_size_bytes: Option<usize>,
    segments: HashMap<String, PathBuf>,
}

// --- Main Logic ---

fn main() -> Result<()> {
    let logger = init_logger()?;

    // Set config_path to 1st arg (If present)
    let args: Vec<String> = env::args().collect();
    let config_path = match args.get(1) {
        Some(path_str) => PathBuf::from(path_str),
        None => PathBuf::from(CONFIG_PATH),
    };

    // ---- Process config ---- //
    let config_str = fs::read_to_string(&config_path)
        .context(format!("Failed to read config file: {:?}", config_path))?;
    let Config {
        output_path,
        root_path,
        post_script,
        log_file,
        compression_level,
        max_size_bytes,
        segments,
    } = toml::from_str(&config_str).context("Failed to parse config TOML")?;

    if let Some(log_file) = log_file {
        set_log_path(&logger, &log_file, LOG_LEVEL)?;
    }

    let output_path = match output_path {
        Some(dir) => dir,
        None => PathBuf::from("/tmp")
    };

    // Setup output directory
    if let Some(dir) = output_path.parent() {
        if !dir.exists() {
            return Err(anyhow!("Output directory not found: {:?}", dir));
        }
    }
    if !output_path.exists() {
        fs::create_dir(&output_path).context("Failed to create output directory")?;
    }

    let all_paths: HashSet<&PathBuf> = segments.values().collect();

    // ---- Process each section ---- //
    for (name, path) in &segments {
        info!("--- Processing Section: {} at {:?} ---", name, path);
        if !path.exists() {
            error!("Path not found, skipping: {:?}", path);
            continue;
        }

        // List paths to exclude from the current segment
        let exclusions: Vec<&PathBuf> = all_paths.iter()
            .filter(|&other_path| {
                path != *other_path && other_path.starts_with(path)
            })
            .copied()
            .collect();

        // Create the archive
        let archive_path = output_path.join(format!("{}.tar.gz", name));

        if let Err(e) = create_archive(
            path,
            &archive_path,
            &root_path,
            &exclusions,
            compression_level,
            max_size_bytes,
            post_script.to_owned(),
        ) {
            error!("Failed to create archive for {}: {}", name, e);
            continue;
        }
        info!("Successfully created archive: {:?}", archive_path);
    }

    info!("Backup process finished.");
    Ok(())
}

