use std::fs::{self, OpenOptions, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn init() {
    ENABLED.store(std::env::var("LOOM_LOG").is_ok(), Ordering::Release);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

pub struct Logger {
    file: Mutex<File>,
}

impl Logger {
    pub fn new(name: &str) -> Option<Self> {
        if !is_enabled() {
            return None;
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let dir = PathBuf::from(&home).join(".loom");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.log", name));
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .ok()?;
        Some(Self { file: Mutex::new(file) })
    }

    pub fn log(&self, level: &str, target: &str, msg: std::fmt::Arguments<'_>) {
        use std::time::SystemTime;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut f = self.file.lock().unwrap();
        let _ = writeln!(f, "[{}] [{}] [{}] {}", now, level, target, msg);
    }
}

#[macro_export]
macro_rules! log_info {
    ($logger:expr, $target:expr, $($arg:tt)+) => {
        if let Some(ref l) = $logger {
            l.log("INFO", $target, format_args!($($arg)+));
        }
    };
}

#[macro_export]
macro_rules! log_debug {
    ($logger:expr, $target:expr, $($arg:tt)+) => {
        if let Some(ref l) = $logger {
            l.log("DEBUG", $target, format_args!($($arg)+));
        }
    };
}

#[macro_export]
macro_rules! log_error {
    ($logger:expr, $target:expr, $($arg:tt)+) => {
        if let Some(ref l) = $logger {
            l.log("ERROR", $target, format_args!($($arg)+));
        }
    };
}
