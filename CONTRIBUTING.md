# Contributing

## Workstreams and ownership

The project is built by parallel workstreams. Each owns a directory; touch another
workstream's directory only through a PR they review.

| Workstream | Owns | Branch prefix |
|---|---|---|
| coordinator | `crates/boba-core`, `docs/`, `README.md`, `llms.txt`, `.github/`, root config | `core/` |
| server | `crates/boba-server` | `server/` |
| runner | `crates/boba-runner` | `runner/` |
| web | `web/` (except `web/src/api/generated/`, which is generated) | `web/` |

**`boba-core` is the contract.** Need a change? Open a small PR labelled `contract` that
only touches `boba-core` (+ regenerated TS), and tell the other workstreams. Additive changes
(new optional field, new enum variant) are preferred over breaking ones.

## Workflow

- Work in your own git worktree, never in the shared checkout:
  `git worktree add ../boba-agent-<stream> -b <prefix>/<topic> origin/main`
- Small PRs into `main`, rebased on `main`. CI must be green.
- Commit messages: imperative, scoped: `server: add ticket move endpoint`.

## Checks

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
cargo test -p boba-core            # regenerates web/src/api/generated/*.ts
cd web && pnpm i && pnpm lint && pnpm typecheck && pnpm build
```

## Running locally

```bash
cd web && pnpm i && pnpm dev       # http://localhost:5173 (proxies /api to :7777)
cargo run -p boba-server           # http://localhost:7777, demo mode without ANTHROPIC_API_KEY
```
