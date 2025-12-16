use anyhow::{Context, Result, anyhow};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::io;
use std::fs;
use log::{info,warn};
use crate::rolling_writer::RollingWriter;

const PATH_FILE: &str = ".seg_bkp.path";

/// Archives a directory, appending a path file and applying exclusions.
pub fn create_archive(
    src_dir: &Path,
    output_path: &Path,
    root_path: &Option<PathBuf>,
    exclusions: &[&PathBuf],
    compression_level: Option<u32>,
    max_size_bytes: Option<usize>,
    script_path: Option<PathBuf>
) -> Result<()> {
    // Configure tar compression
    let comp = match compression_level {
        Some(level) => Compression::new(level),
        None => Compression::default()
    };
    let mut file = RollingWriter::new(output_path.to_path_buf(), max_size_bytes)?;
    if let Some(script) = script_path {
        let callback = move |filename: &String| execute_post_script(script.to_owned(), filename.as_str());
        file.set_listener(callback);
    }
    let enc = GzEncoder::new(file, comp);
    let mut tar = tar::Builder::new(enc);

    // Inject path file into archive
    let path_str = strip_root(src_dir, root_path)?;
    let mut header = tar::Header::new_gnu();
    header.set_path(PATH_FILE)?;
    header.set_size(path_str.len() as u64);
    header.set_mode(0o644);
    header.set_cksum(); // Removing this line will cause the archive to be corrupted
    tar.append(&header, path_str.as_bytes())?;

    append_dir_contents(&mut tar, src_dir, src_dir, exclusions)?;

    tar.finish().context("Failed to finalize tar archive")?;
    let mut writer = tar.into_inner()?.finish().context("Failed to finalize Gzip encoding")?;
    writer.finalize()?;
    Ok(())
}


/// Recursively filter out 'exclusions' while adding files to the archive
fn append_dir_contents(
    tar: &mut tar::Builder<GzEncoder<RollingWriter>>,
    base_dir: &Path,
    current_dir: &Path,
    exclusions: &[&PathBuf],
) -> Result<()> {
    let mut is_empty = current_dir != base_dir;

    for entry in fs::read_dir(current_dir)? {
        is_empty = false;
        let entry = entry?;
        let path = entry.path();

        // Skip already archived paths
        if exclusions.iter().any(|&exclude_path| { path.starts_with(exclude_path) }) {
            info!("Skipping excluded path recursively: {:?}", path);
            continue;
        }

        // Recursively append all files
        if path.is_dir() {
            append_dir_contents(tar, base_dir, &path, exclusions)?;
        } else {
            // Correctly map path relative to the archive root
            let relative_path = path.strip_prefix(base_dir)
                .context(format!("Failed to get relative path for {:?}", path))?;
            tar.append_path_with_name(&path, relative_path)?;
        }
    }

    // Add empty directory to the archive
    if is_empty {
        if let Ok(relative_path) = current_dir.strip_prefix(base_dir) {
            let mut header = tar::Header::new_gnu();
            header.set_path(relative_path)?;
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            header.set_cksum(); // Removing this line will cause the archive to be corrupted
            tar.append(&header, &[] as &[u8])?;
        }
    }
    Ok(())
}


/// Executes an external script, returning exit code.
fn execute_post_script(script_path: PathBuf, arg: &str) -> io::Result<i32> {
    info!("Executing post-script: {:?}", script_path);

    match Command::new(&script_path).arg(arg).status() {
        Ok(status) => {
            let exit_code = match status.code() {
                Some(code) => code,
                None => {
                    if status.success() {
                        0
                    } else {
                        1
                    }
                }
            };

            if exit_code == 0 {
                info!("Post-script finished successfully.");
                Ok(0)
            } else if exit_code < 128 {
                warn!("Post-script finished with error: {}", status);
                Ok(exit_code)
            } else {
                Err(io::Error::new(io::ErrorKind::Other, format!("Post-script panicked: {}", status)))
            }
        }

        Err(e) => {
            if e.kind() == io::ErrorKind::PermissionDenied {
                // Handle common errors
                let can_read = fs::metadata(&script_path).is_ok();
                let error_msg = if can_read {
                    format!("{} is missing execute permission.", script_path.display())
                } else {
                    format!("{} cannot be accessed due to permission issues.", script_path.display())
                };
                return Err(io::Error::new(io::ErrorKind::Other, error_msg))
            }
            return Err(io::Error::new(io::ErrorKind::Other, e.to_string()))
        }
    }
}

/// --- Helper Helpers --- ///

/// Strip the root path from a given path -- extracted to simplify testing
fn strip_root(path: &Path, root_path: &Option<PathBuf>) -> Result<String> {
    Ok(match root_path {
        None => path.to_str()
            .ok_or_else(|| anyhow!("Invalid path string"))?
            .to_string(),
        // Strip root path from source directory (If provided)
        Some(root) => path.strip_prefix(root)
            .context("Invalid root path")?
            .to_str()
            .context("Invalid path string")?
            .to_string(),
    })
}

/// --- Tests --- ///

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_path_stripping_with_root() {
        let src_dir = PathBuf::from("/tmp/files/test_dir");
        let root_path = Some(PathBuf::from("/tmp/files"));
        
        let path_str = strip_root(&src_dir, &root_path).unwrap();
        assert_eq!(path_str, "test_dir");
    }

    #[test]
    fn test_path_stripping_without_root() {
        let src_dir = PathBuf::from("/tmp/files/test_dir");
        let root_path: Option<PathBuf> = None;
        
        let path_str = strip_root(&src_dir, &root_path).unwrap();
        assert_eq!(path_str, "/tmp/files/test_dir");
    }

    #[test]
    fn test_path_stripping_nested() {
        let src_dir = PathBuf::from("/tmp/files/nested/deep/path");
        let root_path = Some(PathBuf::from("/tmp/files"));
        
        let path_str = strip_root(&src_dir, &root_path).unwrap();
        assert_eq!(path_str, "nested/deep/path");
    }

    #[test]
    fn test_path_stripping_exact_match() {
        let src_dir = PathBuf::from("/tmp/files");
        let root_path = Some(PathBuf::from("/tmp/files"));
        
        let path_str = strip_root(&src_dir, &root_path).unwrap();
        assert!(path_str == "");
    }
}

