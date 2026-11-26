# AGENTS.md

`bhr` (browser history reading): a Rust CLI/TUI that turns Chrome history into a
per-day list of pages read, each with the time spent on that URL (`30s` / `15min 20s` / `1h 18min 3s`). Ported from `../kyoten/bin/reading.ts`, but standalone
(no kyoten allowlist-from-posts, no writing into kyoten).

## Commands

```sh
cargo build
cargo test            # filter tests live in src/config.rs, render/duration tests in src/history.rs
cargo test -- --ignored   # hits the real Jev API (needs TYPESAFE_API_KEY); uses made-up pages, never real history
cargo clippy
cargo install --path . --force   # reinstall ~/.cargo/bin/bhr
```

When testing against real history, pass `--config <scratch path>` so the user's
`~/.config/bhr/config.toml` is not touched (it is created automatically if missing).

## Layout

- `src/main.rs` — clap CLI: `tui` (default), `report` (alias `list`), `searches` (search terms from `keyword_search_terms`, not filtered), `hosts`, `config`; global `--config/--since/--until/--json`; `report --ask` ranks with Jev and adds `score` to JSON
- `src/config.rs` — `Config` (TOML: `allow`/`deny`/`tracking_params`/`min_score`/`max_visit_min`), `normalize()` (URL cleanup + all filtering), `host_of()`, `allowed()`, `add_deny()` (edits TOML with `toml_edit` to keep comments)
- `src/history.rs` — find profiles, copy `History` to temp, query SQLite, dedupe per day, sum `visit_duration` per URL (capped by `max_visit_min`), keep the first visit time of the day, Markdown `render()`; `visits()` reads raw visits, `dur()` formats `30s`/`15min 20s`/`1h 18min 3s`, `hms()` formats `H:MM:SS`, `Sort`/`sort_links()` order a day by first-open time, duration or host
- `src/tui.rs` — ratatui two-pane UI (days / pages); `/` text filter, `?` semantic ranking
- `src/ask.rs` — `relevance()`: one Jev `noul` request per page via the `jev` crate (crates.io `polidog-jev`, renamed to `jev` in Cargo.toml), provider from `JEV_PROVIDER`
- `src/default_config.toml` — written to the config path on first run; also used by tests

## Rules

- All filtering goes through `Config::normalize()`; don't filter elsewhere.
- Always-on exclusions (localhost, IPs, auth subdomains, top pages, iframe visits) are hardcoded; everything site-specific belongs in the config, not in code.
- Dedupe key within a day is host + title (not URL), keeping the shortest URL. `Link.order` is the first-open order that day (SQL uses `ORDER BY v.visit_time`, so insertion order = time order).
- Duration is kept per URL, not merged across the dedupe key: same-URL visits add up, different URLs keep their own time. `visit_duration` is µs since 1601 in the `visits` table and can reach days for tabs left open, so each visit is capped at `max_visit_min` (default 60, 0 = no cap).
- UI strings, comments, the default config and the docs are in Simplified Chinese (README.md is now Chinese; the old README.ja.md was replaced by README.zh.md).
- Chrome `visit_time` is microseconds since 1601-01-01; conversion is done in SQL (`date(... 'unixepoch', 'localtime')`).
- Keep dependencies minimal; reuse what is in `Cargo.toml`.

## Ideas not done yet

- Firefox (`places.sqlite`: `moz_places` + `moz_historyvisits`, µs since 1970, skip `visit_type = 8`)
- Safari (`~/Library/Safari/History.db`: `history_items` + `history_visits`, seconds since 2001, needs Full Disk Access)