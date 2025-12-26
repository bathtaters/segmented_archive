pub(crate) mod rolling_writer;
pub(crate) mod logger;
pub(crate) mod hasher;
pub(crate) mod helpers;

use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::path::{PathBuf};
use std::fs;
use std::env;
use log::{info, error, LevelFilter};
use crate::logger::{init_logger, set_log_path};
use crate::hasher::{compute_segment_hash, read_hash_file, write_hash_file};
use crate::helpers::{create_archive, build_ignore_matcher, execute_script};

// --- Structs ---

const CONFIG_PATH: &str = "config.toml"; // Default
const LOG_LEVEL: LevelFilter = LevelFilter::Info;
const CRASH_ON_HASH_FAILURE: bool = false;

#[derive(Debug, serde::Deserialize)]
struct Config {
    output_path: Option<PathBuf>,
    root_path: Option<PathBuf>,
    post_script: Option<PathBuf>,
    skip_script: Option<PathBuf>,
    hash_file: Option<PathBuf>,
    log_file: Option<PathBuf>,
    compression_level: Option<u32>,
    max_size_bytes: Option<usize>,
    segments: HashMap<String, PathBuf>,
    ignore: Option<Vec<String>>,
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
        skip_script,
        hash_file,
        log_file,
        compression_level,
        max_size_bytes,
        segments,
        ignore,
    } = toml::from_str(&config_str).context("Failed to parse config TOML")?;

    if let Some(log_file) = log_file {
        set_log_path(&logger, &log_file, LOG_LEVEL)?;
    }

    let output_path = match output_path {
        Some(dir) => dir,
        None => PathBuf::from("/tmp")
    };

    // Setup output directory
    if output_path.exists() && !output_path.is_dir() {
        return Err(anyhow!("Output path exists but is not a directory: {:?}", output_path));
    }
    if let Some(dir) = output_path.parent() {
        if !dir.exists() {
            return Err(anyhow!("Output directory not found: {:?}", dir));
        }
    }
    if !output_path.exists() {
        fs::create_dir(&output_path).context("Failed to create output directory")?;
    }

    let all_paths: HashSet<&PathBuf> = segments.values().collect();

    // Build ignore pattern matcher if patterns are provided
    let ignore_matcher = ignore.as_ref()
        .map_or_else(|| Ok(None), |patterns| build_ignore_matcher(patterns))
        .context("Failed to build ignore pattern matcher")?;

    // Load existing hash file
    let mut segment_hashes = if let Some(hash_file) = &hash_file {
        read_hash_file(hash_file).context("Failed to read hash file")?
    } else {
        HashMap::<String, String>::new()
    };

    // ---- Process each section ---- //
    for (name, path) in &segments {
        info!("--- Processing Section: {} at {:?} ---", name, path);
        if !path.exists() {
            error!("Path not found, skipping: {:?}", path);
            continue;
        }

        // Generate archive path
        let archive_path = output_path.join(format!("{}.tar.gz", name));

        // List paths to exclude from the current segment
        let exclusions = get_exclusions(&all_paths, path);

        // Compute and store segment hash
        match compute_segment_hash(path, &exclusions, ignore_matcher.as_ref()) {
            Ok(hash) => {
                if segment_hashes.get(name) == Some(&hash) {
                    info!("Segment '{}' has not changed, skipping", name);
                    if let Some(ref script) = skip_script {
                        // Execute skip_script if provided
                        execute_script(script.clone(), &archive_path.display().to_string())?;
                    }
                    continue;
                } else {
                    info!("Computed new hash for segment '{}'", name);
                }
                segment_hashes.insert(name.clone(), hash.clone());
            }
            Err(e) => {
                error!("Failed to compute hash for segment '{}': {}", name, e);
                if CRASH_ON_HASH_FAILURE {
                    return Err(anyhow!("Failed to compute hash for segment '{}'", name))
                } else {
                    info!("Forcing backup of segment '{}' due to hash failure.", name);
                    segment_hashes.remove(name);
                    // Remove this segment from the hash file so it will be backed up
                    // on the next run (even if unchanged) because it can't be hashed.
                }
            }
        }

        // Create the archive
        if let Err(e) = create_archive(
            path,
            &archive_path,
            &root_path,
            &exclusions,
            ignore_matcher.as_ref(),
            compression_level,
            max_size_bytes,
            post_script.to_owned(),
        ) {
            error!("Failed on segment '{}': {}", name, e);
            return Err(anyhow!("Failed on segment '{}'", name));
        }
        info!("Successfully created archive: {:?}", archive_path);
        
        if let Some(hash_file) = &hash_file {
            if let Err(e) = write_hash_file(hash_file, &segment_hashes) {
                info!("New hashes (You can manually update the hash file if you need to): {:?}", segment_hashes);
                error!("Failed to write new hashes to '{}': {}", hash_file.display(), e);
            } else {
                info!("Updated hash file: {:?}", hash_file);
            }
        }
    }

    info!("Backup process finished.");
    Ok(())
}

/// Calculate paths to exclude -- extracted to simplify testing
fn get_exclusions<'a>(all_paths: &'a HashSet<&PathBuf>, path: &PathBuf) -> Vec<&'a PathBuf> {
    all_paths.iter()
        .filter(|&other_path| { path != *other_path && other_path.starts_with(path) })
        .copied()
        .collect()
}

/// --- Tests --- ///

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_exclusion_logic_no_exclusions() {
        let path1 = PathBuf::from("/tmp/test1");
        let path2 = PathBuf::from("/tmp/test2");
        let all_paths: HashSet<&PathBuf> = [&path1, &path2].iter().copied().collect();
        
        let exclusions = get_exclusions(&all_paths, &path1);
        assert_eq!(exclusions.len(), 0);
    }

    #[test]
    fn test_exclusion_logic_nested_path() {
        let path1 = PathBuf::from("/tmp/test1");
        let path2 = PathBuf::from("/tmp/test1/nested");
        let all_paths: HashSet<&PathBuf> = [&path1, &path2].iter().copied().collect();
        
        let exclusions = get_exclusions(&all_paths, &path1);
        assert_eq!(exclusions.len(), 1);
        assert!(exclusions.contains(&&path2));
    }

    #[test]
    fn test_exclusion_logic_deeply_nested() {
        let path1 = PathBuf::from("/tmp/test1");
        let path2 = PathBuf::from("/tmp/test1/nested");
        let path3 = PathBuf::from("/tmp/test1/nested/deep");
        let all_paths: HashSet<&PathBuf> = [&path1, &path2, &path3].iter().copied().collect();
        
        let exclusions = get_exclusions(&all_paths, &path1);
        assert_eq!(exclusions.len(), 2);
        assert!(exclusions.contains(&&path2));
        assert!(exclusions.contains(&&path3));
    }

    #[test]
    fn test_exclusion_logic_sibling_paths() {
        let path1 = PathBuf::from("/tmp/test1");
        let path2 = PathBuf::from("/tmp/test1/sub1");
        let path3 = PathBuf::from("/tmp/test1/sub2");
        let all_paths: HashSet<&PathBuf> = [&path1, &path2, &path3].iter().copied().collect();
        
        let exclusions = get_exclusions(&all_paths, &path1);
        assert_eq!(exclusions.len(), 2);
        assert!(exclusions.contains(&&path2));
        assert!(exclusions.contains(&&path3));
    }

    #[test]
    fn test_exclusion_logic_self_not_excluded() {
        let path1 = PathBuf::from("/tmp/test1");
        let all_paths: HashSet<&PathBuf> = [&path1].iter().copied().collect();
        
        let exclusions = get_exclusions(&all_paths, &path1);
        assert_eq!(exclusions.len(), 0);
    }

    #[test]
    fn test_exclusion_logic_unrelated_paths() {
        let path1 = PathBuf::from("/tmp/test1");
        let path2 = PathBuf::from("/tmp/test2");
        let path3 = PathBuf::from("/tmp/test3");
        let all_paths: HashSet<&PathBuf> = [&path1, &path2, &path3].iter().copied().collect();
        
        let exclusions = get_exclusions(&all_paths, &path1);
        assert_eq!(exclusions.len(), 0);
    }
}

