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
    /// `BOBA_CLAUDE_AUTH=subscription`: the local `claude` is signed in with a personal
    /// Claude subscription (no API key). Its runner is then Personal.
    pub claude_subscription: bool,
    /// `BOBA_LOCAL_RUNNER_OWNER=<email>`: the built-in Claude runner only runs this user's
    /// work (required with a subscription).
    pub local_runner_owner: Option<String>,
    /// `BOBA_GITHUB_WEBHOOK_SECRET`: enables `POST /api/github/webhook` (HMAC-checked).
    pub github_webhook_secret: Option<String>,
    /// GitHub REST API base (`BOBA_GITHUB_API`, GitHub Enterprise: `https://ghe/api/v3`).
    pub github_api: String,
    /// GitHub web base (`BOBA_GITHUB_WEB`), for the app manifest form and clone URLs.
    pub github_web: String,
    /// Where repos added from GitHub are cloned (`BOBA_REPOS_DIR`, default `<data>/repos`).
    pub repos_dir: PathBuf,
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
        let repos_default = data_dir.join("repos");
        // A shared server needs organisation credentials: a personal Claude subscription
        // login must not be shared (Anthropic's terms). Whether the `claude` binary actually
        // works is checked at startup (`main`), which falls back to demo mode otherwise.
        let has_key = env_set("ANTHROPIC_API_KEY")
            || env_set("ANTHROPIC_AUTH_TOKEN")
            || env_set("CLAUDE_CODE_USE_BEDROCK")
            || env_set("CLAUDE_CODE_USE_VERTEX");
        let claude_subscription = std::env::var("BOBA_CLAUDE_AUTH")
            .map(|v| v.trim().eq_ignore_ascii_case("subscription"))
            .unwrap_or(false);
        let demo_mode = env_set("BOBA_DEMO") || !(has_key || claude_subscription);
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
            claude_subscription,
            github_api: std::env::var("BOBA_GITHUB_API")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://api.github.com".into()),
            github_web: std::env::var("BOBA_GITHUB_WEB")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://github.com".into()),
            repos_dir: std::env::var("BOBA_REPOS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| repos_default.clone()),
            github_webhook_secret: std::env::var("BOBA_GITHUB_WEBHOOK_SECRET")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            local_runner_owner: std::env::var("BOBA_LOCAL_RUNNER_OWNER")
                .ok()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty()),
        }
    }

    /// Config for tests: ephemeral data dir, demo mode.
    pub fn for_tests(data_dir: PathBuf) -> Self {
        let repos_default = data_dir.join("repos");
        Self {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            data_dir,
            demo_mode: true,
            dev_cors: None,
            public_url: "http://localhost".into(),
            secure_cookies: false,
            allow_bypass: false,
            claude_subscription: false,
            local_runner_owner: None,
            github_webhook_secret: None,
            github_api: "https://api.github.com".into(),
            github_web: "https://github.com".into(),
            repos_dir: repos_default,
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
