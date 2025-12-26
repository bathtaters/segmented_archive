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

// File permission constants
const FILE_MODE_READ: u32 = 0o644;  // Read-only file permissions (rw-r--r--)

// Exit code threshold for detecting process panics/abnormal termination
// Exit codes >= 128 typically indicate the process was killed by a signal
const PROCESS_EXIT_CODE_THRESHOLD: i32 = 128;

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
        Some(level) => {
            if level > 9 {
                return Err(anyhow!("Compression level must be between 0 and 9: {}", level));
            }
            Compression::new(level)
        },
        None => Compression::default()
    };
    let mut file = RollingWriter::new(output_path.to_path_buf(), max_size_bytes)?;
    if let Some(script) = script_path {
        let callback = move |filename: &String| execute_script(script.to_owned(), filename.as_str());
        file.set_listener(callback);
    }
    let enc = GzEncoder::new(file, comp);
    let mut tar = tar::Builder::new(enc);

    // Inject path file into archive
    let path_str = strip_root(src_dir, root_path)?;
    let mut header = tar::Header::new_gnu();
    header.set_path(PATH_FILE)?;
    header.set_size(path_str.len() as u64);
    header.set_mode(FILE_MODE_READ);
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
            append_file(tar, &path, base_dir)?;
        }
    }

    // Add empty directory to the archive (Except the root, which is added by default)
    if is_empty && current_dir != base_dir {
        if let Ok(relative_path) = current_dir.strip_prefix(base_dir) {
            tar.append_dir(relative_path, current_dir)?;
        }
    }
    Ok(())
}

/// Append a file to the archive
fn append_file(tar: &mut tar::Builder<GzEncoder<RollingWriter>>, path: &Path, base_dir: &Path) -> Result<()> {
    // Correctly map path relative to the archive root
    let relative_path = path.strip_prefix(base_dir)
    .context(format!("Failed to get relative path for {:?}", path))?;

    // Check if this is a symlink
    let metadata = fs::symlink_metadata(&path)
        .context(format!("Failed to read metadata for: {:?}", path))?;

    if metadata.file_type().is_symlink() {
        // Handle symlinks (including broken ones)
        let target = fs::read_link(&path)
            .context(format!("Failed to read symlink target: {:?}", path))?;
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(FILE_MODE_READ);
        tar.append_link(&mut header, relative_path, &target)
            .context(format!("Failed to add symlink to archive: {:?}", path))
    } else {
        // Regular file
        tar.append_path_with_name(&path, relative_path)
            .context(format!("Failed to add file to archive: {:?}", path))
    }
}


/// Executes an external script, returning exit code.
pub fn execute_script(script_path: PathBuf, arg: &str) -> io::Result<i32> {
    info!("Executing script: {:?}", script_path);

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
                info!("Script finished successfully.");
                Ok(0)
            } else if exit_code < PROCESS_EXIT_CODE_THRESHOLD && exit_code > 0 {
                warn!("Script finished with error: {}", status);
                Ok(exit_code)
            } else {
                Err(io::Error::new(io::ErrorKind::Other, format!("Script panicked: {:?}", status)))
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

    #[test]
    fn test_execute_script_success() {
        let test_name = "post_script_success";
        let test_dir = setup_test_dir(test_name);
        
        // Create a simple script that exits with 0
        let script_path = test_dir.join("test_script.sh");
        #[cfg(unix)]
        {
            fs::write(&script_path, "#!/bin/bash\nexit 0\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        {
            // On Windows, create a batch file
            fs::write(&script_path, "@echo off\nexit /b 0\n").unwrap();
        }
        
        let result = execute_script(script_path, "test_arg");
        assert!(result.is_ok(), "Script should execute successfully");
        assert_eq!(result.unwrap(), 0, "Script should return exit code 0");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_execute_script_non_zero_exit() {
        let test_name = "post_script_non_zero";
        let test_dir = setup_test_dir(test_name);
        
        // Create a script that exits with non-zero code
        let script_path = test_dir.join("test_script.sh");
        #[cfg(unix)]
        {
            fs::write(&script_path, "#!/bin/bash\nexit 42\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        {
            fs::write(&script_path, "@echo off\nexit /b 42\n").unwrap();
        }
        
        let result = execute_script(script_path, "test_arg");
        assert!(result.is_ok(), "Script execution should not panic");
        assert_eq!(result.unwrap(), 42, "Script should return exit code 42");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_execute_script_script_not_found() {
        let test_name = "post_script_not_found";
        let test_dir = setup_test_dir(test_name);
        
        // Try to execute a non-existent script
        let script_path = test_dir.join("nonexistent_script.sh");
        
        let result = execute_script(script_path, "test_arg");
        assert!(result.is_err(), "Should return error for non-existent script");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_execute_script_no_execute_permission() {
        let test_name = "post_script_no_exec";
        let test_dir = setup_test_dir(test_name);
        
        // Create a script without execute permission
        let script_path = test_dir.join("test_script.sh");
        fs::write(&script_path, "#!/bin/bash\necho test\n").unwrap();
        
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Remove execute permission
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o644)).unwrap();
            
            let result = execute_script(script_path.clone(), "test_arg");
            assert!(result.is_err(), "Should return error for script without execute permission");
            
            // Verify the error message mentions permission
            let error_msg = result.unwrap_err().to_string();
            assert!(error_msg.contains("execute permission") || error_msg.contains("permission"), 
                "Error should mention permission issue");
        }
        #[cfg(windows)]
        {
            // On Windows, permissions work differently, so this test may not apply
            // Just verify the script can be read
            assert!(fs::metadata(&script_path).is_ok());
        }
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_execute_script_exit_code_above_128() {
        let test_name = "post_script_panic";
        let test_dir = setup_test_dir(test_name);
        
        // Create a script that exits with code > 128 (simulating panic/abnormal termination)
        let script_path = test_dir.join("test_script.sh");
        #[cfg(unix)]
        {
            fs::write(&script_path, "#!/bin/bash\nexit 255\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        {
            // Windows batch files can't easily exit with > 128, so we'll skip this test
            // or use a different approach
            fs::write(&script_path, "@echo off\nexit /b 255\n").unwrap();
        }
        
        let result = execute_script(script_path, "test_arg");
        // The function should return an error for exit codes >= 128
        assert!(result.is_err(), "Should return error for exit code >= 128");
        
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("panicked") || error_msg.contains("255"), 
            "Error should mention panic or the exit code");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_execute_script_with_argument() {
        let test_name = "post_script_arg";
        let test_dir = setup_test_dir(test_name);
        
        // Create a script that writes the argument to a file
        let script_path = test_dir.join("test_script.sh");
        let output_file = test_dir.join("output.txt");
        
        #[cfg(unix)]
        {
            let script_content = format!("#!/bin/bash\necho \"$1\" > {:?}\nexit 0\n", output_file);
            fs::write(&script_path, script_content).unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        {
            let script_content = format!("@echo off\necho %1 > {:?}\nexit /b 0\n", output_file);
            fs::write(&script_path, script_content).unwrap();
        }
        
        let test_arg = "test_argument_value";
        let result = execute_script(script_path, test_arg);
        assert!(result.is_ok(), "Script should execute successfully");
        
        // Verify the argument was passed correctly
        if output_file.exists() {
            let content = fs::read_to_string(&output_file).unwrap();
            assert!(content.contains(test_arg), "Script should receive the argument");
        }
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_empty_base_directory() {
        let test_name = "empty_base_dir";
        let test_dir = setup_test_dir(test_name);
        
        // Create an empty directory (no files, no subdirectories)
        let empty_dir = test_dir.join("empty");
        fs::create_dir(&empty_dir).unwrap();
        
        let archive_path = test_dir.join("empty.tar.gz");
        
        // Should succeed even with empty directory
        create_archive(
            &empty_dir,
            &archive_path,
            &None,
            &[],
            None,
            Some(6),
            None,
            None,
        ).unwrap();
        
        // Archive should exist and be valid
        assert!(archive_path.exists(), "Archive should be created for empty directory");
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // Should contain at least the path file (.seg_arc.path)
        assert!(entries.iter().any(|e| e.contains(".seg_arc.path")), 
            "Archive should contain path file");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_compression_level_validation() {
        let test_name = "compression_validation";
        let test_dir = setup_test_dir(test_name);
        
        // Create a test file
        fs::write(test_dir.join("file.txt"), b"test content").unwrap();
        let archive_path = test_dir.join("test.tar.gz");
        
        // Test valid compression levels (0-9)
        for level in 0..=9 {
            let result = create_archive(
                &test_dir,
                &archive_path,
                &None,
                &[],
                None,
                Some(level),
                None,
                None,
            );
            assert!(result.is_ok(), "Compression level {} should be valid", level);
        }
        
        // Test invalid compression level (> 9)
        let result = create_archive(
            &test_dir,
            &archive_path,
            &None,
            &[],
            None,
            Some(10),
            None,
            None,
        );
        assert!(result.is_err(), "Compression level 10 should be invalid");
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Compression level must be between 0 and 9"), 
            "Error should mention valid range");
        
        // Test very large compression level
        let result = create_archive(
            &test_dir,
            &archive_path,
            &None,
            &[],
            None,
            Some(100),
            None,
            None,
        );
        assert!(result.is_err(), "Compression level 100 should be invalid");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_with_long_path_names() {
        let test_name = "long_paths";
        let test_dir = setup_test_dir(test_name);
        
        // Create a directory structure with a very long path
        let long_path = test_dir.join("TestLongFilePath/TestLongFilePath/TestLongFilePath/TestLongFilePath/TestLongFilePath/TestLongFilePath/LastFolder.Component");
        fs::create_dir_all(&long_path).unwrap();
        
        // Create an empty subdirectory
        let empty_subdir = long_path.join("Contents");
        fs::create_dir(&empty_subdir).unwrap();
        
        // Create a file in the long path
        fs::write(long_path.join("file.txt"), b"test content").unwrap();
        
        // Create another very long path (over 100 characters to test GNU long link support)
        let very_long_path = test_dir.join("A".repeat(50).as_str())
            .join("B".repeat(50).as_str())
            .join("C".repeat(50).as_str());
        fs::create_dir_all(&very_long_path).unwrap();
        fs::write(very_long_path.join("deep_file.txt"), b"deep content").unwrap();
        
        // Create an empty directory in the very long path
        let empty_deep_dir = very_long_path.join("EmptySubdir");
        fs::create_dir(&empty_deep_dir).unwrap();
        
        let archive_path = test_dir.join("test.tar.gz");
        
        // Create archive - this should succeed with long paths
        let result = create_archive(
            &test_dir,
            &archive_path,
            &None,
            &[],
            None,
            Some(6),
            None,
            None,
        );
        
        assert!(result.is_ok(), "Archive creation should succeed with long paths: {:?}", 
            result.err());
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // Verify the long path structure is preserved
        assert!(entries.iter().any(|e| e.contains("LastFolder.Component")), 
            "Archive should contain the long path directory");
        assert!(entries.iter().any(|e| e.contains("LastFolder.Component/Contents")), 
            "Archive should contain the empty subdirectory in long path");
        assert!(entries.iter().any(|e| e.contains("LastFolder.Component/file.txt")), 
            "Archive should contain the file in long path");
        
        // Verify the very long path is preserved
        let has_very_long_path = entries.iter().any(|e| {
            e.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA") ||
            e.contains("deep_file.txt")
        });
        assert!(has_very_long_path, "Archive should contain the very long path");
        
        // Verify empty directories are included
        let has_empty_dir = entries.iter().any(|e| {
            e.contains("EmptySubdir") && !e.contains(".")
        });
        assert!(has_empty_dir, "Archive should contain empty directories in long paths");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_create_archive_with_long_path_names_and_root_path() {
        let test_name = "long_paths_with_root";
        let test_dir = setup_test_dir(test_name);
        
        // Create a base directory structure
        let base_dir = test_dir.join("base");
        fs::create_dir_all(&base_dir).unwrap();
        
        // Create a long path structure
        let long_path = base_dir.join("TestLongFilePath/TestLongFilePath/TestLongFilePath/TestLongFilePath/TestLongFilePath/TestLongFilePath/LastFolder.Component");
        fs::create_dir_all(&long_path).unwrap();
        
        // Create an empty subdirectory
        let empty_subdir = long_path.join("Contents");
        fs::create_dir(&empty_subdir).unwrap();
        
        // Create a file
        fs::write(long_path.join("file.txt"), b"test content").unwrap();
        
        let archive_path = test_dir.join("test.tar.gz");
        
        // Create archive with root_path set (this tests path stripping with long paths)
        let root_path = Some(base_dir.clone());
        let result = create_archive(
            &base_dir,
            &archive_path,
            &root_path,
            &[],
            None,
            Some(6),
            None,
            None,
        );
        
        assert!(result.is_ok(), "Archive creation should succeed with long paths and root_path: {:?}", 
            result.err());
        
        // Extract and verify contents
        let entries = extract_archive_contents(&archive_path);
        
        // With root_path set, the paths should be relative to base_dir
        assert!(entries.iter().any(|e| e.contains("LastFolder.Component")), 
            "Archive should contain the long path directory (relative to root)");
        assert!(entries.iter().any(|e| e.contains("LastFolder.Component/Contents")), 
            "Archive should contain the empty subdirectory");
        assert!(entries.iter().any(|e| e.contains("LastFolder.Component/file.txt")), 
            "Archive should contain the file");
        
        // Verify the path file exists (the exact content depends on root_path logic)
        assert!(entries.iter().any(|e| e.contains(".seg_arc.path")), 
            "Archive should contain path file");
        
        cleanup_test_dir(test_name);
    }
}

