//! 极简日志：格式与 Python 版一致 —— "HH:MM:SS LEVEL-7 message"，输出到 stderr。

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};

use chrono::Local;

pub const DEBUG: u8 = 10;
pub const INFO: u8 = 20;
pub const WARNING: u8 = 30;
pub const ERROR: u8 = 40;

static LEVEL: AtomicU8 = AtomicU8::new(INFO);

pub fn set_level(level: u8) {
    LEVEL.store(level, Ordering::Relaxed);
}

/// 与 Python logging 一致：无法识别的级别回落到 INFO。
pub fn parse_level(name: &str) -> u8 {
    match name.trim().to_ascii_uppercase().as_str() {
        "DEBUG" => DEBUG,
        "INFO" => INFO,
        "WARN" | "WARNING" => WARNING,
        "ERROR" => ERROR,
        "CRITICAL" | "FATAL" => 50,
        _ => INFO,
    }
}

fn level_name(level: u8) -> &'static str {
    match level {
        DEBUG => "DEBUG",
        INFO => "INFO",
        WARNING => "WARNING",
        50 => "CRITICAL",
        _ => "ERROR",
    }
}

pub fn log(level: u8, msg: &str) {
    if level < LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let stamp = Local::now().format("%H:%M:%S");
    let stderr = std::io::stderr();
    let mut handle = stderr.lock();
    let _ = writeln!(handle, "{stamp} {:<7} {}", level_name(level), msg);
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::DEBUG, &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::INFO, &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::WARNING, &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::ERROR, &format!($($arg)*)) };
}
