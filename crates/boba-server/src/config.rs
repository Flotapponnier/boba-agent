//! Runtime configuration, read from the environment.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub addr: SocketAddr,
    pub data_dir: PathBuf,
    /// True when Claude Code cannot run (no API key or no `claude` binary): runs are simulated.
    pub demo_mode: bool,
    /// Origin allowed by CORS in development (`BOBA_DEV_CORS`).
    pub dev_cors: Option<String>,
    /// Public base URL used to build invite links (`BOBA_PUBLIC_URL`).
    pub public_url: String,
    /// Mark the session cookie `Secure` (default: public URL is https).
    pub secure_cookies: bool,
    /// `BOBA_ALLOW_BYPASS=1`: admins may give agents `bypassPermissions` (containers only).
    pub allow_bypass: bool,
}

impl Config {
    pub fn from_env() -> Self {
        let port = std::env::var("BOBA_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(7777u16);
        let host: IpAddr = std::env::var("BOBA_HOST")
            .ok()
            .and_then(|h| h.parse().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let data_dir = std::env::var("BOBA_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));
        let has_key = env_set("ANTHROPIC_API_KEY")
            || env_set("CLAUDE_CODE_USE_BEDROCK")
            || env_set("CLAUDE_CODE_USE_VERTEX");
        let demo_mode = env_set("BOBA_DEMO") || !(has_key && claude_on_path());
        let public_url = std::env::var("BOBA_PUBLIC_URL")
            .unwrap_or_else(|_| format!("http://localhost:{port}"))
            .trim_end_matches('/')
            .to_string();
        let secure_cookies = std::env::var("BOBA_SECURE_COOKIES")
            .map(|v| v == "1" || v == "true")
            .unwrap_or_else(|_| public_url.starts_with("https://"));
        Self {
            addr: SocketAddr::new(host, port),
            data_dir,
            demo_mode,
            dev_cors: std::env::var("BOBA_DEV_CORS")
                .ok()
                .filter(|s| !s.is_empty()),
            public_url,
            secure_cookies,
            allow_bypass: env_set("BOBA_ALLOW_BYPASS"),
        }
    }

    /// Config for tests: ephemeral data dir, demo mode.
    pub fn for_tests(data_dir: PathBuf) -> Self {
        Self {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            data_dir,
            demo_mode: true,
            dev_cors: None,
            public_url: "http://localhost".into(),
            secure_cookies: false,
            allow_bypass: false,
        }
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }
}

fn env_set(key: &str) -> bool {
    std::env::var(key)
        .map(|v| !v.trim().is_empty() && v != "0")
        .unwrap_or(false)
}

fn claude_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join("claude");
        candidate.is_file()
    })
}
