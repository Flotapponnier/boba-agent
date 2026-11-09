# Connect your Claude Code

A **runner** is a machine that executes Boba Agent runs with its own Claude Code. The server ships
with a built-in runner; any teammate can also connect their own machine. It then runs **their**
work (tickets they start, their reviews, jobs they run) with **their** Claude Code login, so a
personal Claude Pro/Max subscription is only ever used by its owner.

The runner dials out to the server over a WebSocket: no open port, works behind NAT and on a laptop.

## 1. Install `boba-agent`

```bash
curl -fsSL https://raw.githubusercontent.com/Flotapponnier/boba-agent/main/install.sh | sh
```

The script picks the build for your OS and CPU (macOS arm64/x86_64, Linux x86_64/arm64),
verifies its SHA-256 against the release's `SHA256SUMS`, and installs it to `~/.local/bin`.
Options: `BOBA_VERSION=v0.1.0` (pin a release), `BOBA_INSTALL_DIR=/usr/local/bin`.

Manual install: download `boba-agent-<target>.tar.gz` and `SHA256SUMS` from the
[releases](https://github.com/Flotapponnier/boba-agent/releases), check with
`shasum -a 256 -c SHA256SUMS --ignore-missing`, extract, and put `boba-agent` on your `PATH`.
Linux builds are static (musl): no glibc version requirements.

## 2. Have Claude Code, git and GitHub access

```bash
npm install -g @anthropic-ai/claude-code
claude auth login          # your own subscription, or set ANTHROPIC_API_KEY
claude auth status         # must say loggedIn: true
gh auth login              # pushes and pull requests (or GH_TOKEN)
```

The runner clones the repos it works on into its data directory (`repos/<owner>-<name>`), so this
machine needs git access to them (SSH key or `gh auth setup-git`).

## 3. Connect

In Boba Agent, open the account menu, **Connect your Claude Code**, create a runner and copy the
command shown (the token is displayed once):

```bash
boba-agent runner --server https://boba.example.com --token <token>
```

| Option | Env | Default |
|---|---|---|
| `--server <url>` | `BOBA_SERVER` | required |
| `--token <token>` | `BOBA_RUNNER_TOKEN` | required |
| `--name <name>` | `BOBA_RUNNER_NAME` | host name |
| `--max-runs <n>` | `BOBA_MAX_CONCURRENT_RUNS` | 2 |
| `--data-dir <dir>` | `BOBA_DATA_DIR` | `./boba-runner-data` |
| `--demo` |  | off (simulated runs, no Claude Code) |

Auth: with `ANTHROPIC_API_KEY` (or Bedrock/Vertex) set, the runner uses the key; otherwise it
uses your `claude auth login` (force either with `BOBA_CLAUDE_AUTH=api_key|subscription`). It checks with
`claude auth status` at startup and refuses to start if Claude Code is not logged in. The server
only routes your own work to a personal runner, and refuses a shared runner on a subscription.

The runner reconnects by itself (backoff up to 1 minute) and keeps runs going while offline:
their updates are delivered when it is back. Ctrl-C or SIGTERM cancels the runs in progress,
flushes their last updates (up to 10 s) and exits.

## 4. Run it in the background

Keep the token out of shell history and unit files readable by others: put it in an env file.

```bash
mkdir -p ~/.config/boba-agent
cat > ~/.config/boba-agent/runner.env <<'ENV'
BOBA_SERVER=https://boba.example.com
BOBA_RUNNER_TOKEN=<token>
BOBA_CLAUDE_AUTH=subscription
BOBA_DATA_DIR=/home/you/.local/share/boba-agent
ENV
chmod 600 ~/.config/boba-agent/runner.env
```

### Linux: systemd user unit

`~/.config/systemd/user/boba-agent-runner.service`:

```ini
[Unit]
Description=Boba Agent runner
After=network-online.target
Wants=network-online.target

[Service]
EnvironmentFile=%h/.config/boba-agent/runner.env
# claude, git and gh must be on this PATH
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=%h/.local/bin/boba-agent runner
Restart=on-failure
RestartSec=10
# Let Boba stop its runs (and their process groups) cleanly.
KillMode=mixed
TimeoutStopSec=20

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now boba-agent-runner
journalctl --user -u boba-agent-runner -f
loginctl enable-linger "$USER"   # keep it running when you are logged out
```

### macOS: launchd agent

launchd does not read env files, so the values go in the plist (keep it `chmod 600`).
`~/Library/LaunchAgents/dev.boba-agent.runner.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.boba-agent.runner</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/you/.local/bin/boba-agent</string>
    <string>runner</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>BOBA_SERVER</key><string>https://boba.example.com</string>
    <key>BOBA_RUNNER_TOKEN</key><string>TOKEN</string>
    <key>BOBA_CLAUDE_AUTH</key><string>subscription</string>
    <key>BOBA_DATA_DIR</key><string>/Users/you/Library/Application Support/boba-agent</string>
    <key>PATH</key><string>/Users/you/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>/Users/you/Library/Logs/boba-agent-runner.log</string>
  <key>StandardErrorPath</key><string>/Users/you/Library/Logs/boba-agent-runner.log</string>
</dict>
</plist>
```

```bash
chmod 600 ~/Library/LaunchAgents/dev.boba-agent.runner.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.boba-agent.runner.plist
launchctl kickstart -k gui/$(id -u)/dev.boba-agent.runner   # restart after an upgrade
tail -f ~/Library/Logs/boba-agent-runner.log
```

`KeepAlive` with `SuccessfulExit = false` restarts the runner after a crash but not after the
server refused it for good (bad token, policy): fix the cause, then `kickstart` it.

## Upgrade and remove

Re-run the install script to upgrade, then restart the service. To remove: stop the service,
delete the runner in Boba Agent (revokes the token), and delete `~/.local/bin/boba-agent` and the
data directory.
