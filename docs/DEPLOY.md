# Deploying Boba Agent

Boba Agent is one container: the Rust server, the web UI, `git`, the GitHub CLI and Claude Code.
State lives in `./data` (SQLite, secret key, worktrees).

## 1. Build and start

```bash
git clone https://github.com/Flotapponnier/boba-agent && cd boba-agent
cp .env.example .env        # then edit it, see below
echo "BOBA_UID=$(id -u)" >> .env && echo "BOBA_GID=$(id -g)" >> .env
docker compose up -d --build
docker compose logs -f boba
```

Open `BOBA_PUBLIC_URL`, create the owner account, then invite teammates from the account menu
(Members → Invite). Access is invite-only.

## 2. Choose how agents run

| Setup | `.env` | Who gets real Claude Code runs |
|---|---|---|
| Demo | nothing | nobody: agents are simulated (good for trying the UI) |
| Organisation API key | `ANTHROPIC_API_KEY=…` | everyone, jobs and auto reviews |
| Your own subscription | `BOBA_CLAUDE_AUTH=subscription`, `BOBA_LOCAL_RUNNER_OWNER=<your email>`, `CLAUDE_HOME=$HOME` | only runs **you** start; teammates' runs are simulated until they connect their own Claude Code |

Anthropic's terms reserve a Claude Pro/Max subscription for its owner's individual use, so Boba
Agent never lets a subscription run someone else's work. Teammates can connect their own machine
("Connect your Claude Code" in the account menu) to run their own work with their own login.

For subscription mode, log in once on the host with `claude` (the container reuses `~/.claude`).

## 3. HTTPS

Put a reverse proxy in front and keep `BOBA_BIND=127.0.0.1`. Caddy:

```
boba.example.com {
    reverse_proxy 127.0.0.1:7777
}
```

nginx (WebSockets need the upgrade headers):

```nginx
server {
    server_name boba.example.com;
    client_max_body_size 12m;
    location / {
        proxy_pass http://127.0.0.1:7777;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 3600s;
    }
    listen 443 ssl; # managed by certbot: sudo certbot --nginx -d boba.example.com
}
```

Set `BOBA_PUBLIC_URL=https://boba.example.com` so invite links and secure cookies are right.

## 4. Repositories and GitHub

Mount the directory holding the repos agents work in (`BOBA_REPOS_DIR`, same path inside and
outside the container) and register them in Integrations with that path. `GH_TOKEN` gives agents
push / PR rights and powers repo sync.

## 5. Backups and upgrades

Back up `./data` (it contains `secret.key`, needed to decrypt connector secrets).
Upgrade with `git pull && docker compose up -d --build`.
