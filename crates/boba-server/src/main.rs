//! Boba Agent server entry point. Owned by the server workstream (see CONTRIBUTING.md).

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    tracing::info!("boba-agent skeleton: server not implemented yet");
    Ok(())
}
