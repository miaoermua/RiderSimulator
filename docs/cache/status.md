# Agent cache: project status

Last verified: 2026-10-09. Specs: `docs/技术设计.md`, `docs/玩法设计.md`.

Three deliverables: **server** (Rust), **control console "烽火台"** (web page served
by the server), **rider app** (Flutter, Android only, no iOS).

## Verified facts

- `cd server && cargo test` → **192 pass** (180 lib + 6 `bin/ledger` + 6
  `tests/acceptance.rs`), 0 fail. Needs `export PATH="$HOME/.cargo/bin:$PATH"`.
- `cargo clippy --all-targets` and `cargo fmt` are clean (no warnings).
- HTTP routes: `/`（烽火台）, `/ws`, `/healthz`, `/api/state`, `/api/dispatch`,
  `/api/config`, `/api/riders/{id}/online`.
- Flutter/Dart are NOT installed on this machine. Client code has never been compiled.
- Empty dir: `server/src/task/` (the task code actually lives in `server/src/sim/task.rs`).

## Server: done

- sim core (zero IO): world, tick, event, order, rider, merchant, scheduler, risk,
  economy, weather, ai (greedy batch + LLM plan validation), trajectory, geo, task, config.
- net: ws + protocol (serde) + hub with per-role filtering (rider / control / spectator).
- net/api: remote control + dispatch over HTTP (`Authorization: Bearer <token>`), routed
  through the same `Inbound` queue as `/ws` so both entries share one `&mut World`.
- amap: client, route (LRU), quota guard, geocode, silent fallback to jittered straight line.
- llm: client, brain (validated assignments, fallback to algorithm), task judge.
- store: SQLite, falls back to in-memory. `Store::days()` lists days with ledgers.
- `src/bin/ledger.rs`: day-ledger CLI (`days` / `day <N>` / `top [N]`, `--json`) —
  closes Phase 1 hard metric #5. Covered by an acceptance test that runs the real binary.
- Control console: single static page `server/static/console.html`, compiled into the
  binary via `include_str!`. Talks to `/ws` as role=control; has rider map, event log,
  config switches, force-assign (派单).
- `.codebuddy/skills/rider-dispatch/`: skill + `references/api.md` + `scripts/dispatch.py`
  for external tools (workbuddy etc.) to drive dispatching over the HTTP API.

## Server: gaps vs spec

1. Content volume: `merchants.json` has 30 (spec 300–500); `tasks.json` has 3 (spec
   suggests 20). No `data/cities.json` (only needed for the multi-city work in §12).
2. Forced assignment flow: spec says the rider gets a confirm dialog and it auto-rejects
   after 3s. **Not implemented** — `force_assign` attaches the order immediately.
   Needs a design decision: 3 real seconds (tick count, scale-dependent) vs sim minutes.
3. Control protocol missing vs §7.3: **world seed** only. Everything else is wired
   (`set_paused`, `set_speed_multiplier`, `set_weather`, `set_commission_rate`,
   `force_assign`, `set_llm_enabled`, `set_config`, `set_rider_online`).
   Seed is deliberately absent: changing it mid-run breaks the same-seed reproducibility
   invariant; it belongs to a "restart the round" feature that does not exist yet.
4. Protocol envelope differs from spec §4.2 (`req/res/evt` with id) — implementation uses
   `type` + `ack`. `packages/protocol.json` documents the actual one. Console and client
   are both built on the current shape; changing it is pure churn. **Decide: keep current,
   and update the spec doc.**
5. No `packages/protocol.ts`; `client/lib/protocol.dart` lives in `client/`, not `packages/`.
   The console is plain JS and does not need it, so this stays a non-gap until a TS client exists.

## Rider app (Flutter): SKELETON ONLY

- Files: `client/lib/{main,game_client,game_state,protocol}.dart`,
  `screens/{login_page,rider_home}.dart`, `widgets/order_card.dart`.
- No platform dirs (`android/` etc.) → needs `flutter create .`.
- Tabs are 大厅 / 我的单 / 战报; spec wants 大厅 / 待取货 / 待送达 / 账单.
- Spec wants swipe-up-to-claim with 3s countdown (red at last second); verify
  `order_card` implements this.
- No map (amap_flutter_plugin) yet; spec allows deferring maps.
- Directory layout differs from spec (`rider/lib/core,models,ws,ui/pages/...`).

## Suggested next steps (priority)

1. Decide the forced-assign confirm semantics, then implement it (server + client).
2. Expand `data/merchants.json` / `data/tasks.json` to spec volume.
3. Install Flutter, `flutter create`, fix compile, restructure tabs to match spec.
4. Update `docs/技术设计.md` §4.2 to describe the actual envelope, or migrate the code.

## Open questions for user

- Forced-assign confirm: 3 real seconds (tick-counted) or N sim minutes?
- Protocol envelope: keep current (`type`/`ack`) and update the spec doc, or migrate?
- Rename `client/` → `rider/` to match spec?
