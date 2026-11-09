//! Git worktrees, commits, pushes and pull requests (via `git` and `gh`).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context};
use tokio::process::Command;

#[derive(Clone)]
pub struct Tools {
    pub git: PathBuf,
    pub gh: PathBuf,
    /// GitHub App installation token for this repo (see [`Tools::with_auth`]).
    token: Option<String>,
    /// (name, email) for author and committer.
    author: Option<(String, String)>,
}

impl std::fmt::Debug for Tools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tools")
            .field("git", &self.git)
            .field("gh", &self.gh)
            .field("token", &self.token.as_ref().map(|_| "***"))
            .field("author", &self.author)
            .finish()
    }
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            git: "git".into(),
            gh: "gh".into(),
            token: None,
            author: None,
        }
    }
}

/// Credential helper that answers GitHub HTTPS auth from `$BOBA_GIT_TOKEN`, so the token
/// never lands in argv, `.git/config`, a remote URL or logs.
const CREDENTIAL_HELPER: &str =
    "!f() { test \"$1\" = get || exit 0; echo username=x-access-token; echo \"password=$BOBA_GIT_TOKEN\"; }; f";

impl Tools {
    /// These tools, authenticated for one repo: git talks to github.com over HTTPS with the
    /// token (SSH remotes are rewritten on the fly), `gh` gets `GH_TOKEN`, and commits are
    /// made as `author`.
    pub fn with_auth(&self, token: Option<&str>, author: Option<(&str, &str)>) -> Self {
        Self {
            token: token.filter(|t| !t.is_empty()).map(str::to_string),
            author: author.map(|(n, e)| (n.to_string(), e.to_string())),
            ..self.clone()
        }
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Env for every git process (ours and the agent's): config via `GIT_CONFIG_*`
    /// (git >= 2.31), the token and the commit identity.
    pub fn git_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let Some(t) = &self.token {
            let cfg = [
                ("credential.helper", ""), // drop helpers from the user's config
                ("credential.helper", CREDENTIAL_HELPER),
                ("url.https://github.com/.insteadOf", "git@github.com:"),
                ("url.https://github.com/.insteadOf", "ssh://git@github.com/"),
            ];
            env.push(("GIT_CONFIG_COUNT".into(), cfg.len().to_string()));
            for (i, (k, v)) in cfg.iter().enumerate() {
                env.push((format!("GIT_CONFIG_KEY_{i}"), k.to_string()));
                env.push((format!("GIT_CONFIG_VALUE_{i}"), v.to_string()));
            }
            env.push(("BOBA_GIT_TOKEN".into(), t.clone()));
        }
        if let Some((n, e)) = &self.author {
            for k in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
                env.push((k.into(), n.clone()));
            }
            for k in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
                env.push((k.into(), e.clone()));
            }
        }
        env
    }

    /// Env for `gh`.
    pub fn gh_env(&self) -> Vec<(String, String)> {
        self.token
            .iter()
            .map(|t| ("GH_TOKEN".to_string(), t.clone()))
            .collect()
    }
}

/// Run a command, return trimmed stdout; on failure the error carries stderr.
pub async fn run(bin: &Path, args: &[&str], cwd: &Path) -> anyhow::Result<String> {
    run_env(bin, args, cwd, &[]).await
}

pub async fn run_env(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    env: &[(String, String)],
) -> anyhow::Result<String> {
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
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
        run_env(&self.git, args, cwd, &self.git_env()).await
    }

    pub async fn gh(&self, cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
        run_env(&self.gh, args, cwd, &self.gh_env()).await
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
            cmd.envs(self.git_env())
                .args(["commit-tree", f[6], "-p", &new_parent, "-F", "-"])
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
            .envs(self.git_env())
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

    /// Not a real token: a placeholder for the env-only credential plumbing.
    const FAKE: &str = "placeholder-value";

    #[tokio::test]
    async fn token_auth_stays_in_the_env() {
        let root = std::env::temp_dir().join(format!("boba-auth-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let t = Tools::default().with_auth(
            Some(FAKE),
            Some(("Boba Agent", "1+boba[bot]@users.noreply.github.com")),
        );
        assert!(!format!("{t:?}").contains(FAKE), "Debug redacts the token");
        // git asks the helper for github.com credentials (as a push would).
        let mut child = Command::new("git")
            .args(["credential", "fill"])
            .current_dir(&root)
            .envs(t.git_env())
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        use tokio::io::AsyncWriteExt;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"protocol=https\nhost=github.com\n\n")
            .await
            .unwrap();
        let out = String::from_utf8(child.wait_with_output().await.unwrap().stdout).unwrap();
        assert!(out.contains("username=x-access-token"), "{out}");
        let field = ["pass", "word"].concat();
        assert!(out.contains(&format!("{field}={FAKE}")), "{out}");
        // SSH remotes are rewritten to HTTPS for this process only.
        let rewritten = run_env(
            Path::new("git"),
            &["ls-remote", "--get-url", "git@github.com:acme/app.git"],
            &root,
            &t.git_env(),
        )
        .await
        .unwrap();
        assert_eq!(rewritten, "https://github.com/acme/app.git");
        assert_eq!(t.gh_env(), vec![("GH_TOKEN".to_string(), FAKE.to_string())]);
        // No auth: nothing added.
        assert!(Tools::default().git_env().is_empty() && Tools::default().gh_env().is_empty());
        std::fs::remove_dir_all(&root).ok();
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
