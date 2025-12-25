use std::io::{self, Write, ErrorKind};
use std::fs::{File, rename};
use std::path::PathBuf;
use log::{info};

/// A custom writer that wraps a file handle and manages rolling over to a new file.
/// 
/// NOTE: 'base_path' will be appended with .part###
pub struct RollingWriter {
    current_file: Option<File>,
    current_path: Option<String>,
    current_size: usize,
    /// If None, all data is written to a single file without part numbering.
    max_size: Option<usize>,
    base_path: PathBuf,
    part_counter: u32,
    rollover_listener: Option<Box<dyn Fn(&String) -> io::Result<i32>>>,
}

impl RollingWriter {
    /// Create a new multi-part file writer
    /// 
    /// # Arguments
    /// * `base_path` - Base path for the output file(s)
    /// * `max_size` - Maximum size per part file in bytes. Must be >= 1 if Some.
    ///                If None, all data is written to a single file.
    /// 
    /// # Errors
    /// Returns an error if `max_size` is `Some(0)` (must be at least 1 byte)
    pub fn new(base_path: PathBuf, max_size: Option<usize>) -> io::Result<Self> {
        if let Some(size) = max_size {
            if size == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "max_size must be at least 1 byte: 0"
                ));
            }
        }
        
        let mut writer = Self {
            current_file: None,
            current_path: None,
            current_size: 0,
            max_size,
            base_path,
            part_counter: 0,
            rollover_listener: None,
        };
        writer.open_new_part()?;
        Ok(writer)
    }

    /// Set a callback function to be called whenever a part is finalized
    pub fn set_listener<F>(&mut self, callback: F)
    where F: Fn(&String) -> io::Result<i32> + 'static {
        self.rollover_listener = Some(Box::new(callback));
    }

    /// Close out any open file part
    pub fn finalize(&mut self) -> io::Result<()> {
        self.finalize_current(true)
    }

    // --- Private methods --- //

    fn open_new_part(&mut self) -> io::Result<()> {
        // Close any open file
        self.finalize_current(false)?;
        
        // Increment part number if max_size is set
        let filename = match self.max_size {
            Some(_) => {
                // Multi-part mode: increment counter and use part number
                self.part_counter += 1;
                format!("{}.part{:03}", self.base_path.display(), self.part_counter)
            }
            None => {
                // Single-file mode: use base path directly
                if self.current_file.is_some() {
                    // This is impossible to reach as long as max_size is immutable
                    return Err(io::Error::new(
                        ErrorKind::Other,
                        "RollingWriter internal error: attempted to open new part in single-file mode with existing file"
                    ));
                }
                self.base_path.display().to_string()
            }
        };
        self.current_path = Some(filename.to_owned());
        
        info!("Opening new file part: {:?}", filename);
        let new_file = File::create(filename)?;
        self.current_file = Some(new_file);
        self.current_size = 0;
        Ok(())
    }

    fn finalize_current(&mut self, is_final: bool) -> io::Result<()> {
        if let Some(mut file) = self.current_file.take() {
            file.flush()?;

            // If there is only 1 part, rename the file to match base_path
            if is_final && self.part_counter == 1 {
                if let Some(filename) = self.current_path.take() {
                    info!("Renaming single part file to {:?}", self.base_path);
                    rename(&filename, &self.base_path)?;
                    self.current_path = Some(self.base_path.display().to_string());
                }
            }
            
            // If a callback is set, call it passing the filename
            if let Some(callback) = &self.rollover_listener {
                if let Some(filename) = &self.current_path {
                    callback(filename)?;
                }
            }
        }
        Ok(())
    }
}

impl Write for RollingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes_written = 0usize;
        let mut bytes_remaining = buf.len();

        while bytes_remaining > 0 {
            // Calculate number of bytes to write
            let write_len = match self.max_size {
                None => bytes_remaining, /* Ignore rollover if max_size is not set */
                Some(max_size) => std::cmp::min(max_size - self.current_size, bytes_remaining),
            };

            // Write next block of data
            let next_write = &buf[bytes_written..(bytes_written + write_len)];
            let written = self.current_file.as_mut()
                .ok_or_else(|| io::Error::new(ErrorKind::Other, "No file handle available"))?
                .write(next_write)?;
            if written != write_len {
                return Err(io::Error::new(ErrorKind::Other, format!(
                    "Unexpected write-size mismatch. Expected: {}, Returned: {}", write_len, written
                )))
            }

            // Update counters
            self.current_size += written;
            bytes_written += written;
            bytes_remaining -= written;

            // Open a new file if there is still data to write
            if bytes_remaining > 0 {
                self.open_new_part()?;
            }
        }

        Ok(bytes_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.current_file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}


/// --- Tests --- ///

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;

    fn get_test_dir(test_name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/rolling_writer_test_{}", test_name))
    }

    fn cleanup_test_dir(test_name: &str) {
        let _ = fs::remove_dir_all(get_test_dir(test_name));
    }

    fn setup_test_dir(test_name: &str) {
        cleanup_test_dir(test_name);
        fs::create_dir_all(&get_test_dir(test_name)).unwrap();
    }

    #[test]
    fn test_rolling_writer_no_max_size() {
        let test_name = "no_max_size";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let mut writer = RollingWriter::new(base_path.clone(), None).unwrap();
        
        let data = b"Hello, World!";
        writer.write_all(data).unwrap();
        writer.finalize().unwrap();
        
        // Should create a single file with the base name (no .part001)
        assert!(base_path.exists());
        let mut contents = Vec::new();
        File::open(&base_path).unwrap().read_to_end(&mut contents).unwrap();
        assert_eq!(contents, data);
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_with_max_size_single_part() {
        let test_name = "single_part";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let mut writer = RollingWriter::new(base_path.clone(), Some(1000)).unwrap();
        
        let data = b"Small data";
        writer.write_all(data).unwrap();
        writer.finalize().unwrap();
        
        // Single part should be renamed to base_path
        assert!(base_path.exists());
        assert!(!get_test_dir(test_name).join("test.tar.gz.part001").exists());
        
        let mut contents = Vec::new();
        File::open(&base_path).unwrap().read_to_end(&mut contents).unwrap();
        assert_eq!(contents, data);
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_with_max_size_multiple_parts() {
        let test_name = "multiple_parts";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let max_size = 100;
        let mut writer = RollingWriter::new(base_path.clone(), Some(max_size)).unwrap();
        
        // Write data that exceeds max_size
        let data = vec![0u8; 250];
        writer.write_all(&data).unwrap();
        writer.finalize().unwrap();
        
        // Should create multiple part files
        assert!(get_test_dir(test_name).join("test.tar.gz.part001").exists());
        assert!(get_test_dir(test_name).join("test.tar.gz.part002").exists());
        assert!(get_test_dir(test_name).join("test.tar.gz.part003").exists());
        
        // Base path should not exist (multiple parts)
        assert!(!base_path.exists());
        
        // Verify total size
        let mut total_size = 0;
        for i in 1..=3 {
            let part_path = get_test_dir(test_name).join(format!("test.tar.gz.part{:03}", i));
            let size = fs::metadata(&part_path).unwrap().len() as usize;
            total_size += size;
        }
        assert_eq!(total_size, 250);
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_exact_max_size_boundary() {
        let test_name = "exact_boundary";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let max_size = 50;
        let mut writer = RollingWriter::new(base_path.clone(), Some(max_size)).unwrap();
        
        // Write exactly max_size bytes
        let data = vec![0u8; max_size];
        writer.write_all(&data).unwrap();
        writer.finalize().unwrap();
        
        // Should create a single part (exactly at boundary)
        assert!(base_path.exists());
        assert!(!get_test_dir(test_name).join("test.tar.gz.part001").exists());
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_spanning_write() {
        let test_name = "spanning";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let max_size = 50;
        let mut writer = RollingWriter::new(base_path.clone(), Some(max_size)).unwrap();
        
        // Write data that spans exactly 2 parts
        let data = vec![0u8; 75];
        writer.write_all(&data).unwrap();
        writer.finalize().unwrap();
        
        // Should create 2 part files
        assert!(get_test_dir(test_name).join("test.tar.gz.part001").exists());
        assert!(get_test_dir(test_name).join("test.tar.gz.part002").exists());
        assert!(!get_test_dir(test_name).join("test.tar.gz.part003").exists());
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_listener_callback() {
        let test_name = "callback";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let max_size = 50;
        let mut writer = RollingWriter::new(base_path.clone(), Some(max_size)).unwrap();
        
        use std::sync::{Arc, Mutex};
        let callback_calls = Arc::new(Mutex::new(Vec::new()));
        let callback_calls_clone = callback_calls.clone();
        writer.set_listener(move |filename| {
            callback_calls_clone.lock().unwrap().push(filename.clone());
            Ok(0)
        });
        
        // Write data that spans multiple parts
        let data = vec![0u8; 120];
        writer.write_all(&data).unwrap();
        writer.finalize().unwrap();
        
        // Callback should be called for each finalized part
        let calls = callback_calls.lock().unwrap();
        assert_eq!(calls.len(), 3); // part001, part002, part003
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_empty_write() {
        let test_name = "empty";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let mut writer = RollingWriter::new(base_path.clone(), Some(100)).unwrap();
        
        // Write empty data
        writer.write_all(&[]).unwrap();
        writer.finalize().unwrap();
        
        // Should still create a file (even if empty)
        assert!(base_path.exists());
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_multiple_writes() {
        let test_name = "multiple_writes";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let max_size = 50;
        let mut writer = RollingWriter::new(base_path.clone(), Some(max_size)).unwrap();
        
        // Write in multiple chunks
        writer.write_all(&vec![0u8; 30]).unwrap();
        writer.write_all(&vec![1u8; 30]).unwrap();
        writer.write_all(&vec![2u8; 30]).unwrap();
        writer.finalize().unwrap();
        
        // Should create 2 parts (30 + 30 + 30 = 90, but first part gets 50, second gets 40)
        assert!(get_test_dir(test_name).join("test.tar.gz.part001").exists());
        assert!(get_test_dir(test_name).join("test.tar.gz.part002").exists());
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_max_size_zero() {
        let test_name = "max_size_zero";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        
        // max_size of 0 should return an error
        let result = RollingWriter::new(base_path.clone(), Some(0));
        assert!(result.is_err(), "max_size of 0 should return error");
        
        if let Err(error) = result {
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("at least 1 byte"), 
                "Error should mention minimum size requirement");
        }
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_max_size_one() {
        let test_name = "max_size_one";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        let mut writer = RollingWriter::new(base_path.clone(), Some(1)).unwrap();
        
        // Write 3 bytes - should create 3 parts
        let data = vec![1u8, 2u8, 3u8];
        writer.write_all(&data).unwrap();
        writer.finalize().unwrap();
        
        // Should create 3 part files
        assert!(get_test_dir(test_name).join("test.tar.gz.part001").exists());
        assert!(get_test_dir(test_name).join("test.tar.gz.part002").exists());
        assert!(get_test_dir(test_name).join("test.tar.gz.part003").exists());
        
        // Verify each part has exactly 1 byte
        for i in 1..=3 {
            let part_path = get_test_dir(test_name).join(format!("test.tar.gz.part{:03}", i));
            let size = fs::metadata(&part_path).unwrap().len() as usize;
            assert_eq!(size, 1, "Part {} should have exactly 1 byte", i);
        }
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_max_size_very_large() {
        let test_name = "max_size_large";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        // Use a very large max_size (1GB)
        let max_size = 1_000_000_000;
        let mut writer = RollingWriter::new(base_path.clone(), Some(max_size)).unwrap();
        
        // Write small amount of data - should all go to single part
        let data = vec![0u8; 1000];
        writer.write_all(&data).unwrap();
        writer.finalize().unwrap();
        
        // Should create single file (renamed to base_path)
        assert!(base_path.exists());
        assert!(!get_test_dir(test_name).join("test.tar.gz.part001").exists());
        
        let size = fs::metadata(&base_path).unwrap().len() as usize;
        assert_eq!(size, 1000, "File should contain all 1000 bytes");
        
        cleanup_test_dir(test_name);
    }

    #[test]
    fn test_rolling_writer_max_size_usize_max() {
        let test_name = "max_size_max";
        setup_test_dir(test_name);
        
        let base_path = get_test_dir(test_name).join("test.tar.gz");
        // Use usize::MAX as max_size (should work, though impractical)
        let max_size = usize::MAX;
        let mut writer = RollingWriter::new(base_path.clone(), Some(max_size)).unwrap();
        
        // Write small amount of data
        let data = vec![0u8; 100];
        writer.write_all(&data).unwrap();
        writer.finalize().unwrap();
        
        // Should create single file
        assert!(base_path.exists());
        
        cleanup_test_dir(test_name);
    }
}
