use anyhow::{Context, Result};
use std::path::{PathBuf};
use std::fs::OpenOptions;
use std::io::Write;
use chrono::Local;
use log::{info, LevelFilter};
use log4rs::Handle;
use log4rs::append::console::ConsoleAppender;
use log4rs::append::file::FileAppender;
use log4rs::config::{Appender, Config as LogConfig, Root};
use log4rs::encode::pattern::PatternEncoder;

/// Setup logging
pub fn init_logger() -> Result<Handle> {
    // Setup console logging
    let stdout = ConsoleAppender::builder().encoder(Box::new(PatternEncoder::new("{h({l})} - {m}\n"))).build();
    let base_config = LogConfig::builder()
        .appender(Appender::builder().build("stdout", Box::new(stdout)))
        .build(Root::builder().appender("stdout").build(LevelFilter::Info))
        .context("Failed to configure base logger")?;
    
    let handle = log4rs::init_config(base_config).context("Failed to start logger")?;
    Ok(handle)
}

/// Reconfigure logger if a log file is specified in config
pub fn set_log_path(log_handle: &Handle, log_path: &PathBuf, log_level: LevelFilter) -> Result<()> {
    let log_path = &replace_placeholders(log_path);
    info!("Saving log to file: {:?}", log_path);

    let file_appender = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new("{d} - {l} - {m}\n")))
        .build(log_path)
        .context("Failed to build file appender")?;

    let file_config = LogConfig::builder()
        .appender(Appender::builder().build("file_log", Box::new(file_appender)))
        .build(Root::builder().appender("file_log").build(log_level))
        .context("Failed to configure file logger")?;

    // Re-initialize logger with the new file configuration
    log_handle.set_config(file_config);
    
    // Write separator line and backup start message to the log file
    if let Ok(mut file) = OpenOptions::new().append(true).open(log_path) {
        let _ = writeln!(file, "--------------------------------");
    }
    info!("Backup process started.");
    
    Ok(())
}

/// Helper function to replace placeholders in a path
pub(crate) fn replace_placeholders(path: &PathBuf) -> PathBuf {
    let now = Local::now();
    let path_str = path.display().to_string()

    // Replace %D w/ Date
        .replace("%D", &now.format("%Y%m%d").to_string());
    
    PathBuf::from(path_str)
}

/// --- Tests --- ///

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use chrono::Local;

    #[test]
    fn test_replace_placeholders_date() {
        let path = PathBuf::from("/tmp/log_%D.log");
        let result = replace_placeholders(&path);
        
        let expected_date = Local::now().format("%Y%m%d").to_string();
        let expected_path = format!("/tmp/log_{}.log", expected_date);
        
        assert_eq!(result, PathBuf::from(expected_path), "Date placeholder should be replaced");
    }

    #[test]
    fn test_replace_placeholders_multiple_date() {
        let path = PathBuf::from("/tmp/%D/log_%D.log");
        let result = replace_placeholders(&path);
        
        let expected_date = Local::now().format("%Y%m%d").to_string();
        let expected_path = format!("/tmp/{}/log_{}.log", expected_date, expected_date);
        
        assert_eq!(result, PathBuf::from(expected_path), "All date placeholders should be replaced");
    }

    #[test]
    fn test_replace_placeholders_no_placeholders() {
        let path = PathBuf::from("/tmp/log.log");
        let result = replace_placeholders(&path);
        
        assert_eq!(result, path, "Path without placeholders should be unchanged");
    }

    #[test]
    fn test_replace_placeholders_consistency() {
        let path = PathBuf::from("/tmp/log_%D.log");
        
        // Call multiple times and verify consistency (within the same second)
        let result1 = replace_placeholders(&path);
        let result2 = replace_placeholders(&path);
        
        assert_eq!(result1, result2, "Placeholder replacement should be consistent within the same second");
    }

    #[test]
    fn test_replace_placeholders_partial_match() {
        // Test that %D in %%D gets replaced (current behavior - simple string replace)
        let path = PathBuf::from("/tmp/log_%%D.log");
        let result = replace_placeholders(&path);
        
        // Current implementation uses simple string replace, so %%D becomes %<date>
        let date_str = Local::now().format("%Y%m%d").to_string();
        let result_str = result.to_string_lossy();
        // The %D inside %%D will be replaced, resulting in %<date>
        assert!(result_str.contains(&date_str), "Date should be inserted even in %%D pattern");
        assert!(result_str.contains("%"), "Should still contain a percent sign");
    }
}