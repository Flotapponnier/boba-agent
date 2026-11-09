//! Process-group handling: every run's `claude` gets its own group so cancellation also
//! kills the tools it spawned (test runners, dev servers…), and nothing outlives the run.

use std::time::Duration;

use tokio::process::Child;

/// Kills the whole process group on drop (SIGKILL), whatever path the run took.
pub struct GroupGuard {
    pgid: Option<i32>,
}

impl GroupGuard {
    pub fn new(child: &Child) -> Self {
        Self {
            pgid: child.id().and_then(|p| i32::try_from(p).ok()),
        }
    }

    fn signal(&self, sig: i32) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            // SAFETY: plain syscall on a group id we created; ESRCH (group gone) is fine.
            unsafe {
                libc::killpg(pgid, sig);
            }
        }
        #[cfg(not(unix))]
        let _ = sig;
    }

    /// SIGTERM the group, give it `grace` to exit, then SIGKILL. Always reaps `child`.
    pub async fn terminate(&self, child: &mut Child, grace: Duration) {
        #[cfg(unix)]
        self.signal(libc::SIGTERM);
        #[cfg(not(unix))]
        let _ = child.start_kill();
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            self.kill();
            let _ = child.kill().await;
        }
        self.kill();
    }

    pub fn kill(&self) {
        #[cfg(unix)]
        self.signal(libc::SIGKILL);
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

// ------------------------------------------------------------------ orphan reaper

/// Written next to each live run so a restarted runner can kill what a crash left behind.
pub const PIDFILE: &str = "pid";

/// Record the run's process group and a marker found in its leader's command line
/// (the Claude session id), as `<pgid>\n<marker>\n`.
pub fn write_pidfile(
    run_dir: &std::path::Path,
    child: &Child,
    marker: &str,
) -> std::io::Result<()> {
    let Some(pid) = child.id() else { return Ok(()) };
    std::fs::create_dir_all(run_dir)?;
    std::fs::write(run_dir.join(PIDFILE), format!("{pid}\n{marker}\n"))
}

#[derive(Debug, Default, PartialEq)]
pub struct ReapReport {
    /// Process groups killed.
    pub killed: Vec<i32>,
    /// Leftover run directories removed (they may hold MCP secrets).
    pub removed_dirs: usize,
}

/// Kill process groups recorded under `<runs_dir>/*/pid` and remove those run dirs.
/// Call once at startup, before accepting runs (every run dir is stale by then).
///
/// A group is killed only if it is still ours: either its leader is alive and its
/// command line contains the recorded marker, or the leader is gone and the group still
/// has members (a pgid cannot be reused while the group exists, so those are our orphans).
pub fn reap_orphans(runs_dir: &std::path::Path) -> ReapReport {
    let mut report = ReapReport::default();
    let Ok(entries) = std::fs::read_dir(runs_dir) else {
        return report;
    };
    for e in entries.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(dir.join(PIDFILE)) {
            let mut lines = raw.lines();
            let pgid = lines.next().and_then(|l| l.trim().parse::<i32>().ok());
            let marker = lines.next().unwrap_or_default().trim().to_string();
            if let Some(pgid) = pgid.filter(|p| *p > 1) {
                if is_ours(pgid, &marker) {
                    GroupGuard { pgid: Some(pgid) }.kill();
                    tracing::warn!(pgid, "killed an orphaned run process group");
                    report.killed.push(pgid);
                }
            }
        }
        if std::fs::remove_dir_all(&dir).is_ok() {
            report.removed_dirs += 1;
        }
    }
    report
}

#[cfg(unix)]
fn is_ours(pgid: i32, marker: &str) -> bool {
    // SAFETY: signal 0 only checks existence / permission.
    let group_alive = unsafe { libc::killpg(pgid, 0) } == 0;
    if !group_alive {
        return false;
    }
    let leader_alive = unsafe { libc::kill(pgid, 0) } == 0;
    if !leader_alive {
        return true;
    }
    let cmd = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pgid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    !marker.is_empty() && cmd.contains(marker)
}

#[cfg(not(unix))]
fn is_ours(_pgid: i32, _marker: &str) -> bool {
    false
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    async fn spawn_group(script: &str) -> Child {
        let mut c = tokio::process::Command::new("bash");
        c.args(["-c", script]).process_group(0).kill_on_drop(false);
        c.spawn().unwrap()
    }

    fn alive(pgid: i32) -> bool {
        unsafe { libc::killpg(pgid, 0) == 0 }
    }

    #[tokio::test]
    async fn reaps_only_our_groups() {
        let root = std::env::temp_dir().join(format!("boba-reap-{}", uuid::Uuid::now_v7()));
        let runs = root.join("runs");
        // Ours: leader command carries the marker (as `claude --session-id <id>` does).
        let mut ours = spawn_group("sleep 300 & wait # marker-abc").await;
        write_pidfile(&runs.join("a"), &ours, "marker-abc").unwrap();
        // Not ours: a live group whose leader does not match the recorded marker (pid reuse).
        let mut other = spawn_group("sleep 302").await;
        write_pidfile(&runs.join("b"), &other, "marker-zzz").unwrap();
        // A stale pidfile for a group that no longer exists, plus a secret left behind.
        std::fs::create_dir_all(runs.join("c")).unwrap();
        std::fs::write(runs.join("c").join(PIDFILE), "999999\nx\n").unwrap();
        std::fs::write(runs.join("c").join("mcp.json"), "{}").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let ours_pg = ours.id().unwrap() as i32;
        let other_pg = other.id().unwrap() as i32;
        let report = reap_orphans(&runs);
        assert_eq!(report.killed, vec![ours_pg]);
        assert_eq!(report.removed_dirs, 3);
        let _ = ours.wait().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !alive(ours_pg),
            "our group (incl. the background sleep) is gone"
        );
        assert!(alive(other_pg), "foreign group untouched");
        GroupGuard {
            pgid: Some(other_pg),
        }
        .kill();
        let _ = other.wait().await;
        std::fs::remove_dir_all(&root).ok();
    }
}
