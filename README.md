# Luminaut

A lightweight, cross-platform DMX monitor for **sACN (E1.31)** and **Art-Net** packets. Built with Tauri and Svelte, it lets you inspect any universe in real time — channel values, levels, and priority — without needing dedicated hardware.

---

## Features

- **Dual protocol** — switch between sACN and Art-Net at any time
- **Interface selection** — bind to a specific network interface or listen on all
- **Universe selector** — navigate universes with +/− buttons or type directly
- **Live 512-channel grid** — colour-coded bar graph with numeric 0–255 values per channel

---

## Stack

| Layer | Technology |
|---|---|
| Desktop shell | [Tauri](https://tauri.app) |
| UI framework | [SvelteKit](https://kit.svelte.dev) + [Svelte](https://svelte.dev) |
| Styling | [Tailwind CSS](https://tailwindcss.com) + [daisyUI](https://daisyui.com) |
| Language (frontend) | TypeScript |
| Language (backend) | Rust |
| sACN protocol | [`sacn`](https://crates.io/crates/sacn) |
| Art-Net protocol | [`artnet_protocol`](https://crates.io/crates/artnet_protocol) |
| Interface enumeration | [`if-addrs`](https://crates.io/crates/if-addrs) |
| Package manager | pnpm |

---

## Prerequisites

| Tool | Install |
|---|---|
| Node.js | [nodejs.org](https://nodejs.org) |
| pnpm | `npm i -g pnpm` |
| Rust | [rustup.rs](https://rustup.rs) |
| Tauri CLI v2 | bundled via `pnpm tauri` |

**Linux only** — install WebKit and related system libraries:

```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  libappindicator3-dev \
  librsvg2-dev \
  patchelf
```

---

## Getting started

```bash
# 1. Clone
git clone https://github.com/your-org/luminaut.git
cd luminaut

# 2. Install frontend dependencies
pnpm install

# 3. Start in development mode (hot-reload frontend + Rust backend)
pnpm tauri dev
```

The app window opens automatically. The Vite dev server runs on `http://localhost:1420`.

---

## Building

```bash
# Production build for the current platform
pnpm tauri build
```

Artifacts land in `src-tauri/target/release/bundle/`.

---

## Project structure

```
luminaut/
├── src/                        # SvelteKit frontend
│   ├── app.css                 # Tailwind + daisyUI imports, global overrides
│   ├── app.html                # HTML shell
│   └── routes/
│       ├── +layout.svelte      # Imports app.css
│       └── +page.svelte        # Main UI (all controls + channel grid)
├── src-tauri/                  # Tauri + Rust backend
│   ├── src/
│   │   └── lib.rs              # All Tauri commands (listeners, interface enum, stop)
│   ├── Cargo.toml
│   └── tauri.conf.json
├── src/generated/tauri/        # Auto-generated TS bindings (do not edit)
│   ├── commands.ts
│   ├── types.ts
│   └── helpers.ts
└── .github/workflows/
    ├── ci.yml                  # Type-check + cargo check + Clippy on every PR
    └── release.yml             # Cross-platform builds triggered by version tags
```

---

## How it works

Tauri exposes Rust functions as IPC commands that the Svelte frontend calls via `invoke`. Each listener runs in a dedicated Rust thread and streams DMX frames back to the frontend through a Tauri `Channel`.

A shared `Arc<AtomicBool>` stop flag lets the frontend cancel any running listener within one poll cycle (≤100 ms). Starting a new listener atomically replaces the flag, so stopping the old thread and starting a new one is race-free.

```
Frontend (Svelte)          Rust (Tauri)
─────────────────          ────────────
sAcnListener(...)   ────▶  spawn thread
                           bind socket
                    ◀────  Channel<DmxFrame> (streaming)
stopListener()      ────▶  AtomicBool → true
                           thread exits on next poll
```

---

## Releases

| Platform | Architecture | Runner |
|---|---|---|
| Linux | x86-64 | `ubuntu-latest` |
| Linux | ARM64 | `ubuntu-24.04-arm` (native) |
| Windows | x86-64 | `windows-latest` |
| Windows | ARM64 | `windows-latest` (cross-compiled via MSVC) |
| macOS | x86-64 (Intel) | `macos-13` |
| macOS | ARM64 (Apple Silicon) | `macos-latest` |

> **Note:** Builds are not yet code-signed. macOS will show a Gatekeeper warning and Windows a SmartScreen prompt on first launch when distributed externally.

---

## Contributing

Found a bug? Have an idea? 😃 Just open an issue or send a PR — all help is genuinely appreciated. 🙏
