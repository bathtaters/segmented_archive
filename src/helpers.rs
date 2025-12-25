use anyhow::{Context, Result, anyhow};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::io;
use std::fs;
use log::{info,warn};
use globset::{GlobSet, GlobSetBuilder};
use crate::rolling_writer::RollingWriter;

const PATH_FILE: &str = ".seg_arc.path";

/// Builds a GlobSet from ignore patterns for efficient pattern matching
pub fn build_ignore_matcher(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(globset::Glob::new(pattern)
            .context(format!("Invalid ignore pattern: {}", pattern))?);
    }
    
    Ok(Some(builder.build()
        .context("Failed to build GlobSet from ignore patterns")?))
}

/// Archives a directory, appending a path file and applying exclusions.
pub fn create_archive(
    src_dir: &Path,
    output_path: &Path,
    root_path: &Option<PathBuf>,
    exclusions: &[&PathBuf],
    ignore_patterns: Option<&GlobSet>,
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

    append_dir_contents(&mut tar, src_dir, src_dir, exclusions, ignore_patterns)?;

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
    ignore_patterns: Option<&GlobSet>,
) -> Result<()> {
    let mut is_empty = true;

    for entry in fs::read_dir(current_dir)? {
        is_empty = false;
        let entry = entry?;
        let path = entry.path();

        // Skip already archived paths
        if is_excluded(&path, exclusions) {
            info!("Skipping excluded path recursively: {:?}", path);
            continue;
        }

        // Check if path matches any ignore pattern
        if let Some(patterns) = ignore_patterns {
            if patterns.is_match(&path) {
                info!("Skipping ignored path: {:?}", path);
                continue;
            }
        }

        // Recursively append all files
        if path.is_dir() {
            append_dir_contents(tar, base_dir, &path, exclusions, ignore_patterns)?;
        } else {
            // Correctly map path relative to the archive root
            let relative_path = path.strip_prefix(base_dir)
                .context(format!("Failed to get relative path for {:?}", path))?;
            tar.append_path_with_name(&path, relative_path)?;
        }
    }

    // Add empty directory to the archive (Except the root, which is added by default)
    if is_empty && current_dir != base_dir {
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

/// Check if a path should be excluded based on the exclusion list
pub fn is_excluded(path: &Path, exclusions: &[&PathBuf]) -> bool {
    exclusions.iter().any(|&exclude_path| path.starts_with(exclude_path))
}

/// --- Tests --- ///

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::fs;
    use flate2::read::GzDecoder;
    use tar::Archive;

    #[test]
    fn test_is_excluded() {
        let path1 = PathBuf::from("/tmp/test1");
        let path2 = PathBuf::from("/tmp/test1/nested");
        let path3 = PathBuf::from("/tmp/test2");
        let path4 = PathBuf::from("/tmp/test1/nested/file.txt");
        
        let exclusions = vec![&path2 as &PathBuf];
        
        // path2 should be excluded (it's in the exclusion list, starts_with returns true for equal paths)
        assert!(is_excluded(&path2, &exclusions));
        
        // path4 should be excluded (it's under path2)
        assert!(is_excluded(&path4, &exclusions));
        
        // path3 should not be excluded (not in list and not under any exclusion)
        assert!(!is_excluded(&path3, &exclusions));
        
        // path1 should not be excluded (it's a parent of an exclusion, not a child)
        assert!(!is_excluded(&path1, &exclusions));
        
        // Test with nested exclusions
        let exclusions2 = vec![&path1 as &PathBuf];
        assert!(is_excluded(&path2, &exclusions2)); // path2 is under path1
        assert!(is_excluded(&path1, &exclusions2)); // path1 starts with itself (equal paths)
    }

    #[test]
    fn test_build_ignore_matcher_empty() {
        let patterns: Vec<String> = vec![];
        let result = build_ignore_matcher(&patterns).unwrap();
        assert!(result.is_none(), "Empty patterns should return None");
    }

    #[test]
    fn test_build_ignore_matcher_single_pattern() {
        let patterns = vec!["*.tmp".to_string()];
        let result = build_ignore_matcher(&patterns).unwrap();
        assert!(result.is_some(), "Valid pattern should return Some(GlobSet)");
        
        let globset = result.unwrap();
        // Test with full paths
        let tmp_path = PathBuf::from("/tmp/test_dir/file.tmp");
        let txt_path = PathBuf::from("/tmp/test_dir/file.txt");
        assert!(globset.is_match(&tmp_path));
        assert!(!globset.is_match(&txt_path));
    }

    #[test]
    fn test_build_ignore_matcher_multiple_patterns() {
        let patterns = vec![
            "*.tmp".to_string(),           // Matches any path ending in .tmp
            "**/.DS_Store".to_string(),    // Matches .DS_Store at any depth
            "**/node_modules".to_string(), // Matches node_modules at any depth
        ];
        let result = build_ignore_matcher(&patterns).unwrap();
        assert!(result.is_some());
        
        let globset = result.unwrap();
        // Test with full paths
        assert!(globset.is_match(&PathBuf::from("/tmp/test_dir/file.tmp")));
        assert!(globset.is_match(&PathBuf::from("/tmp/test_dir/.DS_Store")));
        assert!(globset.is_match(&PathBuf::from("/tmp/test_dir/node_modules")));
        assert!(!globset.is_match(&PathBuf::from("/tmp/test_dir/file.txt")));
    }

    #[test]
    fn test_build_ignore_matcher_invalid_pattern() {
        let patterns = vec!["[invalid".to_string()]; // Invalid glob pattern
        let result = build_ignore_matcher(&patterns);
        assert!(result.is_err(), "Invalid pattern should return error");
    }

    #[test]
    fn test_build_ignore_matcher_recursive_pattern() {
        let patterns = vec!["**/node_modules".to_string()];
        let result = build_ignore_matcher(&patterns).unwrap();
        assert!(result.is_some());
        
        let globset = result.unwrap();
        // Test with full paths
        assert!(globset.is_match(&PathBuf::from("/tmp/test_dir/node_modules")));
        assert!(globset.is_match(&PathBuf::from("/tmp/test_dir/subdir/node_modules")));
        assert!(globset.is_match(&PathBuf::from("/tmp/test_dir/deep/nested/node_modules")));
    }

    #[test]
    fn test_build_ignore_matcher_absolute_path_pattern() {
        let patterns = vec!["/tmp/**".to_string()];
        let result = build_ignore_matcher(&patterns).unwrap();
        assert!(result.is_some());
        
        let globset = result.unwrap();
        // Test with full paths - should match anything under /tmp
        assert!(globset.is_match(&PathBuf::from("/tmp/test_file.txt")));
        assert!(globset.is_match(&PathBuf::from("/tmp/subdir/file.txt")));
        assert!(!globset.is_match(&PathBuf::from("/var/test_file.txt")));
    }

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

    fn get_test_dir(test_name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/helpers_test_{}", test_name))
    }

    fn cleanup_test_dir(test_name: &str) {
        let _ = fs::remove_dir_all(get_test_dir(test_name));
    }

    fn setup_test_dir(test_name: &str) -> PathBuf {
        cleanup_test_dir(test_name);
        let test_dir = get_test_dir(test_name);
        fs::create_dir_all(&test_dir).unwrap();
        test_dir
    }

    fn extract_archive_contents(archive_path: &Path) -> Vec<String> {
        let file = fs::File::open(archive_path).unwrap();
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);
        let mut entries = Vec::new();
        
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let path = entry.path().unwrap();
            entries.push(path.to_string_lossy().to_string());
        }
        entries.sort();
        entries
    }

    #[test]
    fn test_create_archive_with_ignore_patterns_extension() {
        let test_name = "ignore_extensions";
        let test_dir = setup_test_dir(test_name);
        
        // Create test files
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.tmp"), b"content2").unwrap();
        fs::write(test_dir.join("file3.txt"), b"content3").unwrap();
        fs::write(test_dir.join("file4.tmp"), b"content4").unwrap();
        
        // Create archive with ignore pattern for .tmp files
        let patterns = vec!["*.tmp".to_string()];
        let ignore_matcher = build_ignore_matcher(&patterns).unwrap();
        let archive_path = test_dir.join("test.tar.gz");
        
        create_archive(
            &test_dir,
            &archive_path,
            &None,
            &[],
            ignore_matcher.as_ref(),
            Some(6),
            None,
            None,
        ).unwrap();
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // Should contain .txt files but not .tmp files
        assert!(entries.iter().any(|e| e.contains("file1.txt")));
        assert!(entries.iter().any(|e| e.contains("file3.txt")));
        assert!(!entries.iter().any(|e| e.contains("file2.tmp")));
        assert!(!entries.iter().any(|e| e.contains("file4.tmp")));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_with_ignore_patterns_directory() {
        let test_name = "ignore_directory";
        let test_dir = setup_test_dir(test_name);
        
        // Create test structure
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        let node_modules = test_dir.join("node_modules");
        fs::create_dir(&node_modules).unwrap();
        fs::write(node_modules.join("package.json"), b"{}").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        
        // Create archive with ignore pattern for node_modules
        let patterns = vec!["**/node_modules".to_string()];
        let ignore_matcher = build_ignore_matcher(&patterns).unwrap();
        let archive_path = test_dir.join("test.tar.gz");
        
        create_archive(
            &test_dir,
            &archive_path,
            &None,
            &[],
            ignore_matcher.as_ref(),
            Some(6),
            None,
            None,
        ).unwrap();
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // Should contain .txt files but not node_modules
        assert!(entries.iter().any(|e| e.contains("file1.txt")));
        assert!(entries.iter().any(|e| e.contains("file2.txt")));
        assert!(!entries.iter().any(|e| e.contains("node_modules")));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_with_ignore_patterns_hidden_file() {
        let test_name = "ignore_hidden";
        let test_dir = setup_test_dir(test_name);
        
        // Create test files including .DS_Store
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join(".DS_Store"), b"metadata").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        
        // Create archive with ignore pattern for .DS_Store
        let patterns = vec!["**/.DS_Store".to_string()];
        let ignore_matcher = build_ignore_matcher(&patterns).unwrap();
        let archive_path = test_dir.join("test.tar.gz");
        
        create_archive(
            &test_dir,
            &archive_path,
            &None,
            &[],
            ignore_matcher.as_ref(),
            Some(6),
            None,
            None,
        ).unwrap();
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // Should contain .txt files but not .DS_Store
        assert!(entries.iter().any(|e| e.contains("file1.txt")));
        assert!(entries.iter().any(|e| e.contains("file2.txt")));
        assert!(!entries.iter().any(|e| e.contains(".DS_Store")));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_with_ignore_patterns_recursive() {
        let test_name = "ignore_recursive";
        let test_dir = setup_test_dir(test_name);
        
        // Create nested structure with node_modules at different levels
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        let subdir1 = test_dir.join("subdir1");
        fs::create_dir_all(&subdir1).unwrap();
        let node_modules1 = subdir1.join("node_modules");
        fs::create_dir_all(&node_modules1).unwrap();
        fs::write(node_modules1.join("package.json"), b"{}").unwrap();
        
        let subdir2 = test_dir.join("subdir2");
        fs::create_dir_all(&subdir2).unwrap();
        let deep = subdir2.join("deep");
        fs::create_dir_all(&deep).unwrap();
        let node_modules2 = deep.join("node_modules");
        fs::create_dir_all(&node_modules2).unwrap();
        fs::write(node_modules2.join("package.json"), b"{}").unwrap();
        fs::write(subdir2.join("file2.txt"), b"content2").unwrap();
        
        // Create archive with recursive ignore pattern for node_modules
        let patterns = vec!["**/node_modules".to_string()];
        let ignore_matcher = build_ignore_matcher(&patterns).unwrap();
        let archive_path = test_dir.join("test.tar.gz");
        
        create_archive(
            &test_dir,
            &archive_path,
            &None,
            &[],
            ignore_matcher.as_ref(),
            Some(6),
            None,
            None,
        ).unwrap();
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // Should contain .txt files but not any node_modules
        assert!(entries.iter().any(|e| e.contains("file1.txt")));
        assert!(entries.iter().any(|e| e.contains("file2.txt")));
        assert!(!entries.iter().any(|e| e.contains("node_modules")));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_with_ignore_patterns_multiple() {
        let test_name = "ignore_multiple";
        let test_dir = setup_test_dir(test_name);
        
        // Create test files
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.tmp"), b"content2").unwrap();
        fs::write(test_dir.join(".DS_Store"), b"metadata").unwrap();
        let node_modules = test_dir.join("node_modules");
        fs::create_dir(&node_modules).unwrap();
        fs::write(node_modules.join("package.json"), b"{}").unwrap();
        
        // Create archive with multiple ignore patterns
        let patterns = vec![
            "*.tmp".to_string(),
            "**/.DS_Store".to_string(),
            "**/node_modules".to_string(),
        ];
        let ignore_matcher = build_ignore_matcher(&patterns).unwrap();
        let archive_path = test_dir.join("test.tar.gz");
        
        create_archive(
            &test_dir,
            &archive_path,
            &None,
            &[],
            ignore_matcher.as_ref(),
            Some(6),
            None,
            None,
        ).unwrap();
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // Should only contain file1.txt
        assert!(entries.iter().any(|e| e.contains("file1.txt")));
        assert!(!entries.iter().any(|e| e.contains("file2.tmp")));
        assert!(!entries.iter().any(|e| e.contains(".DS_Store")));
        assert!(!entries.iter().any(|e| e.contains("node_modules")));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_with_ignore_patterns_and_exclusions() {
        let test_name = "ignore_with_exclusions";
        let test_dir = setup_test_dir(test_name);
        
        // Create test structure
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        let excluded_dir = test_dir.join("excluded");
        fs::create_dir(&excluded_dir).unwrap();
        fs::write(excluded_dir.join("file2.txt"), b"content2").unwrap();
        fs::write(test_dir.join("file3.tmp"), b"content3").unwrap();
        
        // Create archive with both exclusions and ignore patterns
        let patterns = vec!["*.tmp".to_string()];
        let ignore_matcher = build_ignore_matcher(&patterns).unwrap();
        let exclusions = vec![&excluded_dir as &PathBuf];
        let archive_path = test_dir.join("test.tar.gz");
        
        create_archive(
            &test_dir,
            &archive_path,
            &None,
            &exclusions,
            ignore_matcher.as_ref(),
            Some(6),
            None,
            None,
        ).unwrap();
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // Should only contain file1.txt (excluded dir and .tmp files are skipped)
        assert!(entries.iter().any(|e| e.contains("file1.txt")));
        assert!(!entries.iter().any(|e| e.contains("excluded")));
        assert!(!entries.iter().any(|e| e.contains("file3.tmp")));
        
        cleanup_test_dir(test_name);
    }
}

