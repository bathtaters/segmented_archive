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
    max_size: Option<usize>,
    base_path: PathBuf,
    part_counter: u32,
    rollover_listener: Option<Box<dyn Fn(&String) -> io::Result<i32>>>,
}

impl RollingWriter {
    /// Create a new multi-part file writer
    pub fn new(base_path: PathBuf, max_size: Option<usize>) -> io::Result<Self> {
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
        let filename = if self.max_size.is_some() {
            self.part_counter += 1;
            format!("{}.part{:03}", self.base_path.display(), self.part_counter)
        } else if self.current_file.is_none() {
            self.base_path.display().to_string()
        } else {
            return Err(io::Error::new(
                ErrorKind::Other,
                "RollingWriter in an invalid state: 'max_size' is not set and 'current_file' is. This can happen if 'max_size' is not modified after the RollingWriter has started writing."
            ))
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

            // Open a new file if 
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
