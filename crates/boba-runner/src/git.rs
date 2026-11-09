//! Git worktrees, commits, pushes and pull requests (via `git` and `gh`).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct Tools {
    pub git: PathBuf,
    pub gh: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            git: "git".into(),
            gh: "gh".into(),
        }
    }
}

/// Run a command, return trimmed stdout; on failure the error carries stderr.
pub async fn run(bin: &Path, args: &[&str], cwd: &Path) -> anyhow::Result<String> {
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("could not run {}", bin.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        bail!(
            "`{} {}` failed ({}): {}",
            bin.display(),
            args.join(" "),
            out.status,
            if err.is_empty() {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            } else {
                err.to_string()
            }
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Tools {
    pub async fn git(&self, cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
        run(&self.git, args, cwd).await
    }

    pub async fn gh(&self, cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
        run(&self.gh, args, cwd).await
    }

    async fn ok(&self, cwd: &Path, args: &[&str]) -> bool {
        self.git(cwd, args).await.is_ok()
    }

    pub async fn has_remote(&self, repo: &Path) -> bool {
        self.git(repo, &["remote"])
            .await
            .map(|r| r.lines().any(|l| l == "origin"))
            .unwrap_or(false)
    }

    /// Fetch `origin` if there is one. Errors are returned so callers can log them.
    pub async fn fetch(&self, repo: &Path) -> anyhow::Result<()> {
        if self.has_remote(repo).await {
            self.git(repo, &["fetch", "--prune", "origin"]).await?;
        }
        Ok(())
    }

    /// The ref new branches start from: `origin/<default>` when it exists.
    pub async fn base_ref(&self, repo: &Path, default_branch: &str) -> String {
        let remote = format!("origin/{default_branch}");
        if self
            .ok(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/remotes/{remote}"),
                ],
            )
            .await
        {
            remote
        } else {
            default_branch.to_string()
        }
    }

    pub async fn remove_worktree(&self, repo: &Path, path: &Path) -> anyhow::Result<()> {
        let p = path.to_string_lossy();
        if self
            .git(repo, &["worktree", "remove", "--force", &p])
            .await
            .is_err()
            && path.exists()
        {
            std::fs::remove_dir_all(path).with_context(|| format!("removing {p}"))?;
        }
        let _ = self.git(repo, &["worktree", "prune"]).await;
        Ok(())
    }

    /// Worktree at `path` on `branch` (created from `base` if it does not exist locally or on
    /// origin). An existing worktree already on that branch is reused (keeps a failed run's
    /// work); anything else at `path` is replaced.
    pub async fn work_worktree(
        &self,
        repo: &Path,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> anyhow::Result<bool> {
        if path.exists() {
            let head = self
                .git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
                .await
                .ok();
            if head.as_deref() == Some(branch) {
                return Ok(true);
            }
            self.remove_worktree(repo, path).await?;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let p = path.to_string_lossy();
        if self
            .ok(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .await
        {
            self.git(repo, &["worktree", "add", &p, branch]).await?;
        } else if self
            .ok(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/remotes/origin/{branch}"),
                ],
            )
            .await
        {
            self.git(
                repo,
                &[
                    "worktree",
                    "add",
                    "--track",
                    "-b",
                    branch,
                    &p,
                    &format!("origin/{branch}"),
                ],
            )
            .await?;
        } else {
            self.git(
                repo,
                &["worktree", "add", "--no-track", "-b", branch, &p, base],
            )
            .await?;
        }
        Ok(false)
    }

    /// Detached worktree at the PR head (`pull/<n>/head`) or at the branch.
    pub async fn review_worktree(
        &self,
        repo: &Path,
        path: &Path,
        branch: &str,
        pr_number: Option<i64>,
    ) -> anyhow::Result<()> {
        if path.exists() {
            self.remove_worktree(repo, path).await?;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut target = None;
        if let (Some(n), true) = (pr_number, self.has_remote(repo).await) {
            if self
                .git(repo, &["fetch", "origin", &format!("pull/{n}/head")])
                .await
                .is_ok()
            {
                target = Some(self.git(repo, &["rev-parse", "FETCH_HEAD"]).await?);
            }
        }
        let target = match target {
            Some(t) => t,
            None if self
                .ok(
                    repo,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/remotes/origin/{branch}"),
                    ],
                )
                .await =>
            {
                format!("origin/{branch}")
            }
            None => branch.to_string(),
        };
        self.git(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                &path.to_string_lossy(),
                &target,
            ],
        )
        .await?;
        Ok(())
    }

    /// Commit whatever the agent left uncommitted. Returns true if a commit was made.
    pub async fn commit_leftovers(&self, wt: &Path, message: &str) -> anyhow::Result<bool> {
        if self.git(wt, &["status", "--porcelain"]).await?.is_empty() {
            return Ok(false);
        }
        self.git(wt, &["add", "-A"]).await?;
        let mut args: Vec<&str> = Vec::new();
        let has_identity = self
            .git(wt, &["config", "user.email"])
            .await
            .is_ok_and(|e| !e.is_empty());
        if !has_identity {
            args.extend([
                "-c",
                "user.name=Boba Agent",
                "-c",
                "user.email=boba-agent@users.noreply.github.com",
            ]);
        }
        args.extend(["commit", "--no-verify", "-m", message]);
        self.git(wt, &args).await?;
        Ok(true)
    }

    pub async fn commits_ahead(&self, wt: &Path, base: &str) -> anyhow::Result<u64> {
        let n = self
            .git(wt, &["rev-list", "--count", &format!("{base}..HEAD")])
            .await?;
        Ok(n.parse().unwrap_or(0))
    }

    /// Rewrite the messages of the commits in `base..HEAD` with `clean`, keeping trees,
    /// authors and dates. Commits whose message does not change keep their hash, so
    /// already-pushed history is untouched. Returns how many commits were rewritten.
    /// Merge commits stop the rewrite (returns an error, nothing is changed).
    pub async fn rewrite_messages(
        &self,
        wt: &Path,
        base: &str,
        clean: impl Fn(&str) -> String,
    ) -> anyhow::Result<usize> {
        let list = self
            .git(
                wt,
                &[
                    "rev-list",
                    "--reverse",
                    "--parents",
                    &format!("{base}..HEAD"),
                ],
            )
            .await?;
        if list.is_empty() {
            return Ok(0);
        }
        let mut parent: Option<String> = None;
        let mut rewritten = 0;
        for line in list.lines() {
            let ids: Vec<&str> = line.split_whitespace().collect();
            let (commit, parents) = (ids[0], &ids[1..]);
            if parents.len() != 1 {
                bail!("{commit} is a merge commit; not rewriting history");
            }
            let msg = self
                .git_raw(wt, &["log", "-1", "--format=%B", commit])
                .await?;
            let new_msg = clean(&msg);
            let new_parent = parent.clone().unwrap_or_else(|| parents[0].to_string());
            if new_msg == msg && new_parent == parents[0] {
                parent = Some(commit.to_string());
                continue;
            }
            let meta = self
                .git(
                    wt,
                    &[
                        "log",
                        "-1",
                        "--format=%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%T",
                        commit,
                    ],
                )
                .await?;
            let f: Vec<&str> = meta.split('\0').collect();
            if f.len() < 7 {
                bail!("unexpected git log output for {commit}");
            }
            let mut cmd = Command::new(&self.git);
            cmd.args(["commit-tree", f[6], "-p", &new_parent, "-F", "-"])
                .current_dir(wt)
                .env("GIT_AUTHOR_NAME", f[0])
                .env("GIT_AUTHOR_EMAIL", f[1])
                .env("GIT_AUTHOR_DATE", f[2])
                .env("GIT_COMMITTER_NAME", f[3])
                .env("GIT_COMMITTER_EMAIL", f[4])
                .env("GIT_COMMITTER_DATE", f[5])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(new_msg.as_bytes()).await?;
            }
            let out = child.wait_with_output().await?;
            if !out.status.success() {
                bail!(
                    "git commit-tree failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            parent = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
            rewritten += 1;
        }
        if rewritten > 0 {
            let new_head = parent.unwrap_or_default();
            // Same tree as before: only the branch ref moves, the working tree is untouched.
            self.git(wt, &["reset", "--soft", &new_head]).await?;
        }
        Ok(rewritten)
    }

    /// Like `git` but keeps stdout untrimmed (commit messages).
    async fn git_raw(&self, cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
        let out = Command::new(&self.git)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()
            .await?;
        if !out.status.success() {
            bail!(
                "`git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn push(&self, wt: &Path, branch: &str) -> anyhow::Result<()> {
        self.git(
            wt,
            &["push", "-u", "origin", &format!("HEAD:refs/heads/{branch}")],
        )
        .await
        .map(|_| ())
    }

    /// Existing open PR for `branch`, or a new one. Returns `(number, url, created)`.
    pub async fn open_pr(
        &self,
        wt: &Path,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<(i64, String, bool)> {
        if let Ok(json) = self
            .gh(wt, &["pr", "view", branch, "--json", "number,url,state"])
            .await
        {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                if v["state"] == "OPEN" {
                    if let (Some(n), Some(u)) = (v["number"].as_i64(), v["url"].as_str()) {
                        return Ok((n, u.to_string(), false));
                    }
                }
            }
        }
        let out = self
            .gh(
                wt,
                &[
                    "pr", "create", "--base", base, "--head", branch, "--title", title, "--body",
                    body,
                ],
            )
            .await?;
        let url = out
            .lines()
            .rev()
            .find(|l| l.contains("/pull/"))
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("gh pr create printed no PR url: {out}"))?
            .to_string();
        let number =
            pr_number_from_url(&url).ok_or_else(|| anyhow::anyhow!("no PR number in {url}"))?;
        Ok((number, url, true))
    }
}

pub fn pr_number_from_url(url: &str) -> Option<i64> {
    url.trim_end_matches('/')
        .rsplit_once("/pull/")?
        .1
        .split(['/', '#', '?'])
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn pr_numbers() {
        assert_eq!(
            pr_number_from_url("https://github.com/a/b/pull/12"),
            Some(12)
        );
        assert_eq!(
            pr_number_from_url("https://github.com/a/b/pull/12/files"),
            Some(12)
        );
        assert_eq!(pr_number_from_url("https://github.com/a/b"), None);
    }

    async fn sh(cwd: &Path, args: &[&str]) {
        run(Path::new("git"), args, cwd).await.unwrap();
    }

    /// A repo with one commit on `main` and a bare `origin`.
    pub(crate) async fn fixture_repo(root: &Path) -> PathBuf {
        let origin = root.join("origin.git");
        let repo = root.join("repo");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        sh(&origin, &["init", "-q", "--bare", "-b", "main"]).await;
        sh(&repo, &["init", "-q", "-b", "main"]).await;
        sh(&repo, &["config", "user.email", "t@example.com"]).await;
        sh(&repo, &["config", "user.name", "T"]).await;
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        sh(&repo, &["add", "."]).await;
        sh(&repo, &["commit", "-q", "-m", "init"]).await;
        sh(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )
        .await;
        sh(&repo, &["push", "-q", "-u", "origin", "main"]).await;
        repo
    }

    #[tokio::test]
    async fn worktree_commit_push_cycle() {
        let root = std::env::temp_dir().join(format!("boba-git-{}", uuid::Uuid::now_v7()));
        let repo = fixture_repo(&root).await;
        let t = Tools::default();
        t.fetch(&repo).await.unwrap();
        let base = t.base_ref(&repo, "main").await;
        assert_eq!(base, "origin/main");

        let wt = root.join("data/worktrees/T-1");
        assert!(!t
            .work_worktree(&repo, &wt, "boba/T-1-x", &base)
            .await
            .unwrap());
        assert_eq!(
            t.git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"])
                .await
                .unwrap(),
            "boba/T-1-x"
        );
        assert!(!t.commit_leftovers(&wt, "nothing").await.unwrap());
        std::fs::write(wt.join("new.txt"), "x").unwrap();
        assert!(t.commit_leftovers(&wt, "T-1: x").await.unwrap());
        assert_eq!(t.commits_ahead(&wt, &base).await.unwrap(), 1);
        t.push(&wt, "boba/T-1-x").await.unwrap();

        // Re-preparing on the same branch reuses the worktree.
        assert!(t
            .work_worktree(&repo, &wt, "boba/T-1-x", &base)
            .await
            .unwrap());
        // After removal, the branch is reused (local branch exists).
        t.remove_worktree(&repo, &wt).await.unwrap();
        assert!(!wt.exists());
        t.work_worktree(&repo, &wt, "boba/T-1-x", &base)
            .await
            .unwrap();
        assert!(wt.join("new.txt").exists());
        t.remove_worktree(&repo, &wt).await.unwrap();

        // Review worktree: detached at the pushed branch.
        let rwt = root.join("data/worktrees/T-1-review");
        t.fetch(&repo).await.unwrap();
        t.review_worktree(&repo, &rwt, "boba/T-1-x", None)
            .await
            .unwrap();
        assert!(rwt.join("new.txt").exists());
        assert_eq!(
            t.git(&rwt, &["rev-parse", "--abbrev-ref", "HEAD"])
                .await
                .unwrap(),
            "HEAD"
        );
        t.remove_worktree(&repo, &rwt).await.unwrap();
        std::fs::remove_dir_all(&root).ok();
    }
}
