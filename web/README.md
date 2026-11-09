# Boba Agent web UI

The game board: React 19 + TypeScript (strict) + Vite, Zustand store, dnd-kit, motion.

```bash
pnpm i
pnpm dev:mock   # http://localhost:5173, in-browser fake server, no backend needed
pnpm dev        # proxies /api (and the WebSocket) to http://localhost:7777
pnpm typecheck && pnpm lint && pnpm build   # build goes to web/dist, embedded by the Rust server
pnpm build:demo # static demo (mock mode) in web/dist-demo, works from any sub-path
```

## Layout

| Path | What |
|---|---|
| `src/api/generated/` | Contract types generated from `crates/boba-core` (never edit by hand) |
| `src/api/` | REST client (`client.ts`), transport (real or mock), the single WebSocket (`connection.ts`: reconnect, resync, subscriptions) |
| `src/store/` | Normalised Zustand store fed by `ServerMessage`, optimistic actions, per-viewer prefs (localStorage), UI state, Fx/toast bus |
| `src/mock/` | Fake server with the same REST + WS semantics and simulated Claude Code runs (`VITE_MOCK=1`). `window.bobaMock` lets you reset or speed it up |
| `src/components/` | Board, tavern, panels (ticket + live run, agent profile, history, skills), modals, FX layer |
| `src/audio/` | Original procedural chiptune (Web Audio) and Bobavim SFX |

Demo login in mock mode: `owner@boba.local` with any password. Add `?fresh` to the URL to see the first-run setup screen.
