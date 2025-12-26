use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

/// Global static to access server configuration
pub static CONFIG: std::sync::LazyLock<Config> = std::sync::LazyLock::new(Config::build);
const DEFAULT_CONFIG_FILENAME: &str = "touch_config.toml";
const DEFAULT_CONFIG_STR: &str = r#"# Storage directory
storage_directory = "db_files/"

# TCP socket to accept connections
tcp_socket = "127.0.0.1:7070"

# Max connection at a time
max_connections = 100

# Allowed values:
# - 1 => Info
# - 2 => Warn
# - 3 => Error
log_level = 1

# Signifies when database can do background merges of parts, depending on database load
background_merge_available_under = 5"#;

/// Server configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Socket for running TCP connection
    tcp_socket: SocketAddrV4,
    /// Storage directory
    storage_directory: PathBuf,
    /// Logging level.
    /// ### Allowed values:
    /// - 1 => Info
    /// - 2 => Warn
    /// - 3 => Error
    log_level: u8,
    /// Max concurrent connections.
    max_concurrent_connections: usize,
    /// Signifies when database can do background merges of parts, depending on database load
    background_merge_available_under: usize,
}

impl Config {
    /// Get TCP socket address from configuration
    pub const fn get_tcp_socket_addr(&self) -> SocketAddrV4 {
        self.tcp_socket
    }

    /// Get database directory from configuration
    pub const fn get_db_dir(&self) -> &PathBuf {
        &self.storage_directory
    }

    /// Get logging level from configuration
    pub const fn get_log_level(&self) -> log::LevelFilter {
        match &self.log_level {
            2 => log::LevelFilter::Warn,
            3 => log::LevelFilter::Error,
            _ => log::LevelFilter::Info,
        }
    }

    /// Get max connections from configuration
    pub const fn get_max_connections(&self) -> usize {
        self.max_concurrent_connections
    }

    /// Provides the background merge availability threshold.
    ///
    /// The threshold value used to determine when background merges are allowed
    /// meaning, that database is under low load
    pub const fn get_background_merge_available_under(&self) -> usize {
        self.background_merge_available_under
    }
    /// Ensures that storage directory exists and is indeed directory. Creates one, if not exists
    ///
    /// # Panics:
    ///
    /// 1. When Permission denied to create a directory
    /// 2. When supplied invalid name
    /// 3. Any `std::fs::create_dir_all()` error
    /// 4. When path already exists, but is not a directory
    fn ensure_storage_directory_exists(dir: &PathBuf) {
        std::fs::create_dir_all(dir).unwrap_or_else(|error| match error.kind() {
            ErrorKind::PermissionDenied => {
                panic!("Permission denied to create database: {error:?}")
            }
            ErrorKind::InvalidInput => panic!("Invalid database name: {error:?}"),
            _ => panic!("Invalid directory: {error:?}"),
        });

        if !std::fs::exists(dir)
            .unwrap_or_else(|error| panic!("Can't check existence of database directory: {error}"))
        {
            panic!("Could not create storage directory ({})", dir.display());
        }

        assert!(
            dir.is_dir(),
            "Database path {} exists but is not a directory.",
            dir.display()
        );
    }

    /// Builds a configuration from environment variables.
    ///
    /// # Panics:
    ///
    /// 1. When `CONFIG_PATH` env var is not set
    /// 2. When `CONFIG_PATH` env var is invalid UTF-8
    /// 2. When config file does not exist
    /// 2. When config file is invalid toml
    pub fn build() -> Self {
        let config_path =
            std::env::var("CONFIG_PATH").unwrap_or(DEFAULT_CONFIG_FILENAME.to_string());
        let config_path = Path::new(&config_path);

        if !config_path.exists() {
            std::fs::write(config_path, DEFAULT_CONFIG_STR)
                .expect("Couldn't write default config file");
        }

        let config_file = std::fs::read_to_string(config_path).expect("Couldn't read config file");
        let raw_config: Self = toml::from_str(&config_file).expect("Invalid config file");

        Self::ensure_storage_directory_exists(&raw_config.storage_directory);

        raw_config
    }
}
