use anyhow::{Context, Result};
use std::path::{PathBuf};
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
    Ok(())
}

/// Helper function to replace placeholders in a path
fn replace_placeholders(path: &PathBuf) -> PathBuf {
    let now = Local::now();
    let path_str = path.display().to_string()

    // Replace %D w/ Date
        .replace("%D", &now.format("%Y%m%d").to_string());
    
    PathBuf::from(path_str)
}