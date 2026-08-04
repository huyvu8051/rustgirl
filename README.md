# RustGirl

[![CI](https://github.com/huyvu8051/rustgirl/actions/workflows/ci.yml/badge.svg)](https://github.com/huyvu8051/rustgirl/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/huyvu8051/rustgirl)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-informational)](#getting-started)

A Postman-style HTTP client desktop app, built entirely in Rust with [egui](https://github.com/emilk/egui)/[eframe](https://github.com/emilk/egui/tree/master/crates/eframe). Native, no Electron/webview — a single small binary that runs on macOS, Windows, and Linux.

## Features

- **Collections & folders** — organize requests, save them, reload with one click.
- **Environments** — `{{variable}}` substitution across URL, params, headers, and body, switchable from the top bar.
- **Request bodies** — None, Raw, JSON, URL-encoded Form, `multipart/form-data` (with native file picker for file fields), and raw Binary file upload.
- **Pre-request / post-response scripting (Lua)** — a `pm.*` API modeled after Postman's, sandboxed via [mlua](https://github.com/mlua-rs/mlua):
  - `pm.environment.get(key)` / `pm.environment.set(key, value)`
  - `pm.request.url` / `.method` / `.body`, `pm.request:getHeader(key)` / `:setHeader(key, value)` — pre-request only
  - `pm.response.status` / `.body` / `.duration_ms` / `.error`, `pm.response:getHeader(key)`, `pm.response:json()` — post-response only
  - `pm.test(name, function() ... end)` for pass/fail assertions
  - `console.log(...)` for debugging
- **History** — every request is recorded with the *exact* resolved request and full response (headers, body, timing) or error — not just a status code. Click an entry to replay what actually happened, or jump straight to it with **Option/Alt+1..9** for your 9 most recent Saved Requests.
- **Response viewer** — syntax-highlighted JSON/HTML, auto-formatting, find-in-body, and a dedicated tab showing exactly what was sent on the wire.
- **Quick-open** — **Option/Alt+Space** to fuzzy-search every request across all collections.
- **Accessible** — built on AccessKit, with proper screen-reader labels on icon-only controls.

## Getting started

Requires Rust 1.85+ (edition 2024).

### Install Rust

If you don't already have Rust, install it via [rustup](https://rustup.rs):

**macOS / Linux**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Windows**

Download and run [`rustup-init.exe`](https://win.rustup.rs), or via winget:

```powershell
winget install Rustlang.Rustup
```

Restart your terminal, then verify the install:

```bash
rustc --version   # should print 1.85.0 or later
cargo --version
```

Already have Rust but on an older version? Update it with `rustup update`.

### Build & run

```bash
git clone https://github.com/huyvu8051/rustgirl.git
cd rustgirl
cargo run --release
```

The Lua interpreter is vendored and built from source by `mlua`, so no system Lua install is required. On Linux you'll need a C compiler toolchain (`build-essential` on Debian/Ubuntu, or the equivalent for your distro) for `mlua`'s vendored build.

## Tech stack

Rust · [egui](https://github.com/emilk/egui)/eframe · [reqwest](https://github.com/seanmonstar/reqwest) · [tokio](https://tokio.rs) · [mlua](https://github.com/mlua-rs/mlua) (Lua 5.4, vendored) · [rfd](https://github.com/PolyMeilex/rfd) · serde

## License

[MIT](LICENSE)
