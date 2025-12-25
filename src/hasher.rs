use anyhow::{Context, Result};
use xxhash_rust::xxh3::Xxh3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::io::{BufReader, BufRead, Write, Read};
use std::fs;
use log::{warn};
use globset::GlobSet;
use crate::helpers::is_excluded;

// Buffer size for reading files during hashing (8KB)
const HASHER_BUFFER_SIZE: usize = 8192;

/// Computes a hash for a segment by hashing all files (excluding folders and exclusions)
/// Uses xxHash (xxh3) for individual files, then XORs all hashes together
/// Includes file paths in the hash to detect renames and moves
pub fn compute_segment_hash(src_dir: &Path, exclusions: &[&PathBuf], ignore_patterns: Option<&GlobSet>) -> Result<String> {
    let mut combined_hash: u64 = 0;
    let mut file_count = 0;

    hash_dir_contents(src_dir, src_dir, exclusions, ignore_patterns, &mut combined_hash, &mut file_count)?;

    // If no files were found, hash an empty string
    if file_count == 0 {
        let mut hasher = Xxh3::new();
        hasher.update(b"");
        combined_hash = hasher.digest();
    }

    // Format as 16-character hex string
    Ok(format!("{:016x}", combined_hash))
}

/// Recursively hash files in a directory, applying the same exclusion logic as tar creation
fn hash_dir_contents(
    base_dir: &Path,
    current_dir: &Path,
    exclusions: &[&PathBuf],
    ignore_patterns: Option<&GlobSet>,
    combined_hash: &mut u64,
    file_count: &mut usize,
) -> Result<()> {
    for entry in fs::read_dir(current_dir)? {
        let entry = entry?;
        let path = entry.path();

        // Skip excluded paths (same logic as append_dir_contents)
        if is_excluded(&path, exclusions) {
            continue;
        }

        // Check if path matches any ignore pattern
        if let Some(patterns) = ignore_patterns {
            if patterns.is_match(&path) {
                continue;
            }
        }

        if path.is_dir() {
            // Recursively process subdirectories
            hash_dir_contents(base_dir, &path, exclusions, ignore_patterns, combined_hash, file_count)?;
        } else {
            // Get relative path to append to the hash
            let relative_path = path.strip_prefix(base_dir)
                .context(format!("Failed to get relative path for {:?}", path))?;
            
            // Hash the file
            let file_hash = hash_file(&path, relative_path)?;
            *combined_hash ^= file_hash;
            *file_count += 1;
        }
    }
    Ok(())
}

/// Hash a single file + its path using xxHash
fn hash_file(file_path: &Path, relative_path: &Path) -> Result<u64> {
    let mut hasher = Xxh3::new();
    
    // Include the relative path in the hash (detects renames and moves)
    // Convert path to string bytes for consistent hashing across platforms
    let path_str = relative_path.to_string_lossy();
    hasher.update(path_str.as_bytes());
    
    // Check if this is a symlink
    let metadata = fs::symlink_metadata(file_path)
        .context(format!("Failed to read metadata for: {:?}", file_path))?;
    
    if metadata.file_type().is_symlink() {
        // For symlinks, hash the target path string (not the target file)
        let target = fs::read_link(file_path)
            .context(format!("Failed to read symlink target: {:?}", file_path))?;
        let target_str = target.to_string_lossy();
        hasher.update(target_str.as_bytes());
    } else {
        // For regular files, hash the file content
        let file = fs::File::open(file_path)
            .context(format!("Failed to open file for hashing: {:?}", file_path))?;
        let mut reader = BufReader::new(file);
        
        let mut buffer = vec![0u8; HASHER_BUFFER_SIZE];
        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }
    }
    
    Ok(hasher.digest())
}

/// Read the hash file into a HashMap
pub fn read_hash_file(hash_file_path: &Path) -> Result<HashMap<String, String>> {
    let mut hashes = HashMap::new();
    
    if !hash_file_path.exists() {
        return Ok(hashes);
    }

    let file = fs::File::open(hash_file_path)
        .context(format!("Failed to open hash file: {:?}", hash_file_path))?;
    let reader = BufReader::new(file);

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.context(format!("Failed to read line {} from hash file", line_num + 1))?;
        let line = line.trim();
        
        // Skip empty lines
        if line.is_empty() {
            continue;
        }

        // Parse key=hash format
        if let Some(equal_pos) = line.find('=') {
            let key = line[..equal_pos].trim().to_string();
            let hash = line[equal_pos + 1..].trim().to_string();
            if hashes.contains_key(&key) {
                warn!("Duplicate key in hash file (Last value is used): {}", key);
            }
            hashes.insert(key, hash);
        } else {
            warn!("Invalid line in hash file (line {}): {}", line_num + 1, line);
        }
    }

    Ok(hashes)
}

/// Write a HashMap to the hash file in key=hash format
pub fn write_hash_file(hash_file_path: &Path, hashes: &HashMap<String, String>) -> Result<()> {
    // Create parent directory if it doesn't exist
    if let Some(parent) = hash_file_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .context(format!("Failed to create directory for hash file: {:?}", parent))?;
        }
    }

    let mut file = fs::File::create(hash_file_path)
        .context(format!("Failed to create hash file: {:?}", hash_file_path))?;

    // Sort keys for consistent output
    let mut sorted_keys: Vec<&String> = hashes.keys().collect();
    sorted_keys.sort();

    for key in sorted_keys {
        if let Some(hash) = hashes.get(key) {
            writeln!(file, "{}={}", key, hash)
                .context(format!("Failed to write to hash file: {:?}", hash_file_path))?;
        }
    }

    file.sync_all()
        .context(format!("Failed to sync hash file: {:?}", hash_file_path))?;

    Ok(())
}

/// --- Tests --- ///

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::fs;
    use std::io::Write;

    fn get_test_dir(test_name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/hasher_test_{}", test_name))
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

    #[test]
    fn test_hash_detects_filename_change() {
        let test_name = "filename_change";
        let test_dir = setup_test_dir(test_name);
        
        // Create file with original name
        let file1 = test_dir.join("original.txt");
        fs::write(&file1, b"same content").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Rename file (same content, different path)
        let file2 = test_dir.join("renamed.txt");
        fs::rename(&file1, &file2).unwrap();
        let hash2 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Hashes should be different (path is included)
        assert_ne!(hash1, hash2, "Hash should change when filename changes");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_detects_file_move() {
        let test_name = "file_move";
        let test_dir = setup_test_dir(test_name);
        
        // Create file in subdirectory
        let subdir1 = test_dir.join("dir1");
        fs::create_dir(&subdir1).unwrap();
        let file1 = subdir1.join("file.txt");
        fs::write(&file1, b"same content").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Move file to different subdirectory
        let subdir2 = test_dir.join("dir2");
        fs::create_dir(&subdir2).unwrap();
        let file2 = subdir2.join("file.txt");
        fs::rename(&file1, &file2).unwrap();
        let hash2 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Hashes should be different (path is included)
        assert_ne!(hash1, hash2, "Hash should change when file is moved");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_detects_content_change() {
        let test_name = "content_change";
        let test_dir = setup_test_dir(test_name);
        
        // Create file with initial content
        let file = test_dir.join("file.txt");
        fs::write(&file, b"original content").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Change file content
        fs::write(&file, b"modified content").unwrap();
        let hash2 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Hashes should be different
        assert_ne!(hash1, hash2, "Hash should change when content changes");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_identical_files_different_paths() {
        let test_name = "identical_files";
        let test_dir = setup_test_dir(test_name);
        
        // Create two identical files in different locations
        let file1 = test_dir.join("dir1").join("file.txt");
        fs::create_dir_all(file1.parent().unwrap()).unwrap();
        fs::write(&file1, b"identical content").unwrap();
        
        let file2 = test_dir.join("dir2").join("file.txt");
        fs::create_dir_all(file2.parent().unwrap()).unwrap();
        fs::write(&file2, b"identical content").unwrap();
        
        let hash = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Edit both files identically
        fs::write(&file1, b"new identical content").unwrap();
        fs::write(&file2, b"new identical content").unwrap();
        let hash_after = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Hashes should be different (different paths = different hashes)
        assert_ne!(hash, hash_after, "Hash should change even if identical files are edited identically");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_empty_segment() {
        let test_name = "empty_segment";
        let test_dir = setup_test_dir(test_name);
        
        // Empty directory should produce a hash (of empty string)
        let hash = compute_segment_hash(&test_dir, &[], None).unwrap();
        assert!(!hash.is_empty(), "Empty segment should produce a hash");
        
        // Hash should be consistent
        let hash2 = compute_segment_hash(&test_dir, &[], None).unwrap();
        assert_eq!(hash, hash2, "Empty segment hash should be consistent");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_exclusions() {
        let test_name = "exclusions";
        let test_dir = setup_test_dir(test_name);
        
        // Create files in main directory
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Create excluded subdirectory
        let excluded_dir = test_dir.join("excluded");
        fs::create_dir(&excluded_dir).unwrap();
        fs::write(excluded_dir.join("file3.txt"), b"content3").unwrap();
        
        // Hash should be the same (excluded files not included)
        let exclusions = vec![&excluded_dir as &PathBuf];
        let hash2 = compute_segment_hash(&test_dir, &exclusions, None).unwrap();
        assert_eq!(hash1, hash2, "Hash should be same when excluded files are added");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_ignore_patterns_extension() {
        let test_name = "ignore_extensions";
        let test_dir = setup_test_dir(test_name);
        
        // Create files in main directory
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Add .tmp files (should be ignored)
        fs::write(test_dir.join("file3.tmp"), b"content3").unwrap();
        fs::write(test_dir.join("file4.tmp"), b"content4").unwrap();
        
        // Build ignore matcher for .tmp files
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("*.tmp").unwrap());
        let ignore_matcher = Some(builder.build().unwrap());
        
        // Hash should be the same (ignored files not included)
        let hash2 = compute_segment_hash(&test_dir, &[], ignore_matcher.as_ref()).unwrap();
        assert_eq!(hash1, hash2, "Hash should be same when ignored .tmp files are added");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_ignore_patterns_directory() {
        let test_name = "ignore_directory";
        let test_dir = setup_test_dir(test_name);
        
        // Create files in main directory
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Add node_modules directory (should be ignored)
        let node_modules = test_dir.join("node_modules");
        fs::create_dir(&node_modules).unwrap();
        fs::write(node_modules.join("package.json"), b"{}").unwrap();
        fs::write(node_modules.join("index.js"), b"console.log('test');").unwrap();
        
        // Build ignore matcher for node_modules
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("**/node_modules").unwrap());
        let ignore_matcher = Some(builder.build().unwrap());
        
        // Hash should be the same (ignored directory not included)
        let hash2 = compute_segment_hash(&test_dir, &[], ignore_matcher.as_ref()).unwrap();
        assert_eq!(hash1, hash2, "Hash should be same when ignored node_modules is added");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_ignore_patterns_hidden_file() {
        let test_name = "ignore_hidden";
        let test_dir = setup_test_dir(test_name);
        
        // Create files in main directory
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Add .DS_Store file (should be ignored)
        fs::write(test_dir.join(".DS_Store"), b"metadata").unwrap();
        
        // Build ignore matcher for .DS_Store
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("**/.DS_Store").unwrap());
        let ignore_matcher = Some(builder.build().unwrap());
        
        // Hash should be the same (ignored file not included)
        let hash2 = compute_segment_hash(&test_dir, &[], ignore_matcher.as_ref()).unwrap();
        assert_eq!(hash1, hash2, "Hash should be same when ignored .DS_Store is added");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_ignore_patterns_recursive() {
        let test_name = "ignore_recursive";
        let test_dir = setup_test_dir(test_name);
        
        // Create files in main directory
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Add node_modules at different nesting levels
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
        
        // Build ignore matcher for recursive node_modules
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("**/node_modules").unwrap());
        let ignore_matcher = Some(builder.build().unwrap());
        
        // Hash should be the same (ignored directories not included)
        let hash2 = compute_segment_hash(&test_dir, &[], ignore_matcher.as_ref()).unwrap();
        assert_eq!(hash1, hash2, "Hash should be same when ignored recursive node_modules are added");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_ignore_patterns_multiple() {
        let test_name = "ignore_multiple";
        let test_dir = setup_test_dir(test_name);
        
        // Create files in main directory
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Add multiple types of files that should be ignored
        fs::write(test_dir.join("file3.tmp"), b"content3").unwrap();
        fs::write(test_dir.join(".DS_Store"), b"metadata").unwrap();
        let node_modules = test_dir.join("node_modules");
        fs::create_dir(&node_modules).unwrap();
        fs::write(node_modules.join("package.json"), b"{}").unwrap();
        
        // Build ignore matcher with multiple patterns
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("*.tmp").unwrap());
        builder.add(globset::Glob::new("**/.DS_Store").unwrap());
        builder.add(globset::Glob::new("**/node_modules").unwrap());
        let ignore_matcher = Some(builder.build().unwrap());
        
        // Hash should be the same (all ignored files/dirs not included)
        let hash2 = compute_segment_hash(&test_dir, &[], ignore_matcher.as_ref()).unwrap();
        assert_eq!(hash1, hash2, "Hash should be same when multiple ignored patterns are added");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_ignore_patterns_and_exclusions() {
        let test_name = "ignore_with_exclusions";
        let test_dir = setup_test_dir(test_name);
        
        // Create files in main directory
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Add both excluded directory and ignored files
        let excluded_dir = test_dir.join("excluded");
        fs::create_dir(&excluded_dir).unwrap();
        fs::write(excluded_dir.join("file2.txt"), b"content2").unwrap();
        fs::write(test_dir.join("file3.tmp"), b"content3").unwrap();
        
        // Build ignore matcher for .tmp files
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("*.tmp").unwrap());
        let ignore_matcher = Some(builder.build().unwrap());
        let exclusions = vec![&excluded_dir as &PathBuf];
        
        // Hash should be the same (both excluded and ignored items not included)
        let hash2 = compute_segment_hash(&test_dir, &exclusions, ignore_matcher.as_ref()).unwrap();
        assert_eq!(hash1, hash2, "Hash should be same when both exclusions and ignore patterns are used");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_ignore_patterns_affects_hash_when_ignored_file_changes() {
        let test_name = "ignore_changes";
        let test_dir = setup_test_dir(test_name);
        
        // Create files
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.tmp"), b"content2").unwrap();
        
        // Build ignore matcher for .tmp files
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("*.tmp").unwrap());
        let ignore_matcher = Some(builder.build().unwrap());
        
        let hash1 = compute_segment_hash(&test_dir, &[], ignore_matcher.as_ref()).unwrap();
        
        // Change ignored file (should not affect hash)
        fs::write(test_dir.join("file2.tmp"), b"different content").unwrap();
        let hash2 = compute_segment_hash(&test_dir, &[], ignore_matcher.as_ref()).unwrap();
        assert_eq!(hash1, hash2, "Hash should not change when ignored file changes");
        
        // Change non-ignored file (should affect hash)
        fs::write(test_dir.join("file1.txt"), b"different content").unwrap();
        let hash3 = compute_segment_hash(&test_dir, &[], ignore_matcher.as_ref()).unwrap();
        assert_ne!(hash1, hash3, "Hash should change when non-ignored file changes");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_consistency() {
        let test_name = "consistency";
        let test_dir = setup_test_dir(test_name);
        
        // Create same file structure
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();
        let subdir = test_dir.join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("file3.txt"), b"content3").unwrap();
        
        // Hash should be consistent across multiple calls
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        let hash2 = compute_segment_hash(&test_dir, &[], None).unwrap();
        assert_eq!(hash1, hash2, "Hash should be consistent for same directory");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_hash_file_missing() {
        let test_name = "read_missing";
        let missing_file = get_test_dir(test_name).join("nonexistent.hash");
        
        let hashes = read_hash_file(&missing_file).unwrap();
        assert!(hashes.is_empty(), "Reading missing hash file should return empty HashMap");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_write_hash_file() {
        let test_name = "read_write";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file
        let mut hashes = HashMap::new();
        hashes.insert("segment1".to_string(), "abc123".to_string());
        hashes.insert("segment2".to_string(), "def456".to_string());
        write_hash_file(&hash_file, &hashes).unwrap();
        
        // Read it back
        let read_hashes = read_hash_file(&hash_file).unwrap();
        assert_eq!(read_hashes.len(), 2);
        assert_eq!(read_hashes.get("segment1"), Some(&"abc123".to_string()));
        assert_eq!(read_hashes.get("segment2"), Some(&"def456".to_string()));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_hash_file_with_empty_lines() {
        let test_name = "read_empty_lines";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file with empty lines
        let mut file = fs::File::create(&hash_file).unwrap();
        writeln!(file, "segment1=abc123").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "segment2=def456").unwrap();
        writeln!(file, "   ").unwrap();
        writeln!(file, "segment3=ghi789").unwrap();
        file.sync_all().unwrap();
        
        // Read it back (empty lines should be skipped)
        let read_hashes = read_hash_file(&hash_file).unwrap();
        assert_eq!(read_hashes.len(), 3);
        assert_eq!(read_hashes.get("segment1"), Some(&"abc123".to_string()));
        assert_eq!(read_hashes.get("segment2"), Some(&"def456".to_string()));
        assert_eq!(read_hashes.get("segment3"), Some(&"ghi789".to_string()));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_write_hash_file_sorted() {
        let test_name = "write_sorted";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file with unsorted keys
        let mut hashes = HashMap::new();
        hashes.insert("zebra".to_string(), "hash1".to_string());
        hashes.insert("apple".to_string(), "hash2".to_string());
        hashes.insert("banana".to_string(), "hash3".to_string());
        write_hash_file(&hash_file, &hashes).unwrap();
        
        // Read file content and verify it's sorted
        let content = fs::read_to_string(&hash_file).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[0], "apple=hash2");
        assert_eq!(lines[1], "banana=hash3");
        assert_eq!(lines[2], "zebra=hash1");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_symlink_target() {
        let test_name = "symlink_target";
        let test_dir = setup_test_dir(test_name);
        
        // Create target files
        let target1 = test_dir.join("target1.txt");
        let target2 = test_dir.join("target2.txt");
        fs::write(&target1, b"content1").unwrap();
        fs::write(&target2, b"content2").unwrap();
        
        // Create symlink pointing to target1
        let symlink_path = test_dir.join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target1, &symlink_path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target1, &symlink_path).unwrap();
        
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Remove old symlink and create new one pointing to target2
        fs::remove_file(&symlink_path).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target2, &symlink_path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target2, &symlink_path).unwrap();
        
        let hash2 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Hash should change when symlink target changes
        assert_ne!(hash1, hash2, "Hash should change when symlink target changes");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_symlink_path() {
        let test_name = "symlink_path";
        let test_dir = setup_test_dir(test_name);
        
        // Create target file
        let target = test_dir.join("target.txt");
        fs::write(&target, b"content").unwrap();
        
        // Create symlink with one name
        let symlink1 = test_dir.join("link1.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &symlink1).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &symlink1).unwrap();
        
        let hash1 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Remove old symlink and create new one with different name (same target)
        fs::remove_file(&symlink1).unwrap();
        let symlink2 = test_dir.join("link2.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &symlink2).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &symlink2).unwrap();
        
        let hash2 = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Hash should change when symlink path changes (even if target is same)
        assert_ne!(hash1, hash2, "Hash should change when symlink path changes");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_hash_broken_symlink() {
        let test_name = "broken_symlink";
        let test_dir = setup_test_dir(test_name);
        
        // Create a regular file for comparison
        let regular_file = test_dir.join("regular.txt");
        fs::write(&regular_file, b"content").unwrap();
        let hash_with_regular = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Create a broken symlink (pointing to non-existent file)
        let broken_symlink = test_dir.join("broken_link.txt");
        let non_existent_target = test_dir.join("nonexistent.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&non_existent_target, &broken_symlink).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&non_existent_target, &broken_symlink).unwrap();
        
        // Hash should succeed even with broken symlink (hashes the target path string)
        let hash_with_broken = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Hash should be different (broken symlink adds a new path)
        assert_ne!(hash_with_regular, hash_with_broken, "Hash should change when broken symlink is added");
        
        // Hash should be consistent across multiple calls
        let hash_with_broken2 = compute_segment_hash(&test_dir, &[], None).unwrap();
        assert_eq!(hash_with_broken, hash_with_broken2, "Hash should be consistent for broken symlink");
        
        // Change the broken symlink target path (still broken, but different target)
        fs::remove_file(&broken_symlink).unwrap();
        let different_target = test_dir.join("different_nonexistent.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&different_target, &broken_symlink).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&different_target, &broken_symlink).unwrap();
        
        let hash_with_different_broken = compute_segment_hash(&test_dir, &[], None).unwrap();
        
        // Hash should change when symlink target path changes (even if both are broken)
        assert_ne!(hash_with_broken, hash_with_different_broken, "Hash should change when broken symlink target path changes");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_hash_file_malformed_line() {
        let test_name = "hash_malformed";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file with malformed line (no equals sign)
        let mut file = fs::File::create(&hash_file).unwrap();
        writeln!(file, "segment1=abc123").unwrap();
        writeln!(file, "malformed_line_no_equals").unwrap();
        writeln!(file, "segment2=def456").unwrap();
        file.sync_all().unwrap();
        
        // Should read valid entries and warn about invalid line
        let hashes = read_hash_file(&hash_file).unwrap();
        assert_eq!(hashes.len(), 2, "Should read 2 valid entries");
        assert_eq!(hashes.get("segment1"), Some(&"abc123".to_string()));
        assert_eq!(hashes.get("segment2"), Some(&"def456".to_string()));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_hash_file_duplicate_keys() {
        let test_name = "hash_duplicate";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file with duplicate keys (last one wins)
        let mut file = fs::File::create(&hash_file).unwrap();
        writeln!(file, "segment1=abc123").unwrap();
        writeln!(file, "segment1=def456").unwrap();
        writeln!(file, "segment2=ghi789").unwrap();
        file.sync_all().unwrap();
        
        // Should read entries (last value for duplicate key wins)
        let hashes = read_hash_file(&hash_file).unwrap();
        assert_eq!(hashes.len(), 2, "Should have 2 unique keys");
        assert_eq!(hashes.get("segment1"), Some(&"def456".to_string()), "Last value should win");
        assert_eq!(hashes.get("segment2"), Some(&"ghi789".to_string()));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_hash_file_very_long_line() {
        let test_name = "hash_long_line";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file with very long line (10KB key, 10KB value)
        let long_key = "a".repeat(10000);
        let long_value = "b".repeat(10000);
        let mut file = fs::File::create(&hash_file).unwrap();
        writeln!(file, "{}={}", long_key, long_value).unwrap();
        writeln!(file, "segment2=normal").unwrap();
        file.sync_all().unwrap();
        
        // Should handle long lines without issues
        let hashes = read_hash_file(&hash_file).unwrap();
        assert_eq!(hashes.len(), 2, "Should read both entries");
        assert_eq!(hashes.get(&long_key), Some(&long_value));
        assert_eq!(hashes.get("segment2"), Some(&"normal".to_string()));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_hash_file_empty_key() {
        let test_name = "hash_empty_key";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file with empty key
        let mut file = fs::File::create(&hash_file).unwrap();
        writeln!(file, "=abc123").unwrap();
        writeln!(file, "segment2=def456").unwrap();
        file.sync_all().unwrap();
        
        // Should handle empty key (though unusual)
        let hashes = read_hash_file(&hash_file).unwrap();
        assert_eq!(hashes.len(), 2, "Should read both entries");
        assert_eq!(hashes.get(""), Some(&"abc123".to_string()));
        assert_eq!(hashes.get("segment2"), Some(&"def456".to_string()));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_hash_file_empty_value() {
        let test_name = "hash_empty_value";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file with empty value
        let mut file = fs::File::create(&hash_file).unwrap();
        writeln!(file, "segment1=").unwrap();
        writeln!(file, "segment2=def456").unwrap();
        file.sync_all().unwrap();
        
        // Should handle empty value
        let hashes = read_hash_file(&hash_file).unwrap();
        assert_eq!(hashes.len(), 2, "Should read both entries");
        assert_eq!(hashes.get("segment1"), Some(&"".to_string()));
        assert_eq!(hashes.get("segment2"), Some(&"def456".to_string()));
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_read_hash_file_multiple_equals() {
        let test_name = "hash_multiple_equals";
        let test_dir = setup_test_dir(test_name);
        let hash_file = test_dir.join("test.hash");
        
        // Write hash file with multiple equals signs (first one is delimiter)
        let mut file = fs::File::create(&hash_file).unwrap();
        writeln!(file, "segment1=abc=123=xyz").unwrap();
        writeln!(file, "segment2=def456").unwrap();
        file.sync_all().unwrap();
        
        // Should use first equals as delimiter
        let hashes = read_hash_file(&hash_file).unwrap();
        assert_eq!(hashes.len(), 2, "Should read both entries");
        assert_eq!(hashes.get("segment1"), Some(&"abc=123=xyz".to_string()), 
            "Value should include all content after first equals");
        assert_eq!(hashes.get("segment2"), Some(&"def456".to_string()));
        
        cleanup_test_dir(test_name);
    }
}
