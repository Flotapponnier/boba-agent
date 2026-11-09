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
