# SpinDex

Offline-first DJ library manager for Windows only. Version 0.3.0.

SpinDex scans your music collection on internal drives and USB devices, indexes it into a fast searchable cache, detects duplicates, analyzes BPM/key/quality, and manages metadata and crates — including Serato crates. Everything runs locally: no cloud, no accounts, no internet required.

**Core principle:** your audio files and their tags are always the source of truth. The SQLite database is only a cache and search index. It can be deleted and rebuilt at any time without losing anything.

## Quick start — run the app

```sh
npm install        # once
npm run tauri dev  # opens the SpinDex window
```

The first run compiles the Rust backend and takes a few minutes; after that it starts in seconds. In the window, click **Add library** in the sidebar, pick any folder with audio files, watch the scan progress, then search away.

The app runs fine with nothing else installed — the extra downloads below only unlock analysis, playback of aiff, and two optional features.

### Optional components

```powershell
# ffmpeg, into %APPDATA%\app.spindex.desktop\bin
powershell -ExecutionPolicy Bypass -File scripts\install-components.ps1

# ML audio restoration (large download, optional, see Licensing)
powershell -ExecutionPolicy Bypass -File scripts\setup-apollo.ps1
```

For development you can instead put ffmpeg in `src-tauri/binaries/` with `scripts\fetch-ffmpeg.ps1`; that directory is searched first. The BPM and key models are fetched with `scripts\fetch-models.ps1` plus a one-time `python scripts\convert-key-model.py` (needs Python 3.12 + torch) — the installer ships them already converted, so only developers need this step.

**Analyze stays disabled until the models and ffmpeg are present**; ML Restore stays hidden until Apollo is set up. See [What SpinDex installs, and what you install yourself](#what-spindex-installs-and-what-you-install-yourself).

## Features

- **Library scanning** — walks folders, reads tags (MP3, FLAC, WAV, AIFF, AAC, M4A), fast incremental rescans that skip unchanged files, and flags files that disappeared from disk
- **Instant search** — full-text search over title, artist, album, genre, label, comments *and file/folder names* (SQLite FTS5)
- **USB devices** — removable drives are detected on insertion, indexed as one library per top-level folder, and recognised again on reconnect even across drive-letter changes (volume-serial identity). Eject from the sidebar
- **Preview player** — double-click to play in the bottom deck: play/pause (spacebar), prev/next, shuffle and repeat, click-to-seek waveform coloured by frequency band, volume
- **BPM & key analysis** — on-demand, via ffmpeg (installed separately) plus a Rust ML sidecar ([Beat This!](https://github.com/CPJKU/beat_this) for tempo, OpenKeyScan for key in Camelot notation). Runs in parallel with progress and cancel. Low-confidence keys are flagged, alternatives are offered, and a key you set by hand is locked against re-analysis
- **Audio quality assessment** — integrated LUFS, clipping ratio, spectral cutoff and a 0–100 score per track, with a spectrogram panel that shows where a lossy transcode was cut off
- **Enhancement & repair** — one-click loudness normalization of quiet masters, tempo stabilization of drifting tracks, and optional ML restoration of lossy-damaged audio. All three write a new file by default, or replace with a first-touch backup you can always restore
- **Duplicate detection** — four levels (filename, normalized artist+title, metadata similarity, SHA-256) with a review-and-cleanup modal that suggests which copy to keep and never lets you delete every copy of a track
- **Metadata editing** — single-track and batch (opt-in per field), written into the files themselves via [lofty](https://crates.io/crates/lofty), so edits survive rescans and work in other software. Optional one-click online lookup fills missing tags
- **Crates** — every subfolder of a library is a crate, mirrored live from disk. Create, rename, delete, drag tracks between them, copy a whole crate to another device, and order tracks by hand
- **Serato integration** — read and edit `_Serato_` crates: add, remove, reorder, create, rename, delete, plus a sync tool that detects and repairs a Serato-side overwrite. Every write makes a first-touch backup first
- **Rekordbox crates** — read-only: SpinDex parses a connected device's `export.pdb` and shows its playlist/folder tree in the sidebar, click to filter the table. No editing
- **Visualizer** — a fullscreen WebGL audio visualizer with ten scenes, driven by the analyzed BPM and key

Deferred beyond the MVP: audio fingerprinting for duplicate detection, rating tags, editable Rekordbox crates.

## The machine-learning parts, in plain language

Three of SpinDex's features are powered by **machine-learning models**. If that phrase means nothing to you, here is the whole idea in one paragraph:

> A model is not a program that someone wrote rules for. Nobody could write down the rules for "where is the beat in this track" — real music speeds up, drops out, swings, and buries the kick under everything else. Instead, researchers took many thousands of songs where humans had already marked the answer, and let a program adjust itself, over and over, until its answers matched the humans'. What you end up with is a **file full of numbers** — the model — that has no idea what music *is*, but has become very good at recognising the patterns it was shown. SpinDex feeds your audio in and reads the answer out.

Three things worth knowing before you rely on any of it:

- **Everything runs on your own computer.** Your music is never uploaded and no account is needed. The models are files on your disk, like any other file.
- **No graphics card is needed.** They run on the CPU. That is why analysis takes seconds per track rather than being instant, and why restoration takes half an hour.
- **The answers are estimates, not facts.** A very good estimate, usually — but SpinDex shows you when it is unsure rather than pretending otherwise, and anything you correct by hand is locked so it never gets overwritten.

### Beat This! — finds the tempo (BPM)

Trained by a research group at Johannes Kepler University Linz. It listens to the whole track and marks where it thinks each beat falls; SpinDex turns the spacing between those beats into a BPM number. Because it marks *every* beat rather than just producing one average number, SpinDex can also tell you whether the tempo is **steady or drifting** — useful for old vinyl rips and live recordings, and the basis of the *Steady tempo* feature that gently stretches a wobbly track onto a constant grid.

### OpenKeyScan — finds the musical key

Trained by rekordcloud. It reports the key in **Camelot notation** (`8A`, `5B`, …), the wheel system DJs use for harmonic mixing: neighbouring numbers sound good together, and `A`/`B` are the minor/major pair of the same key.

Key detection is genuinely ambiguous on some music — a filtered disco loop can honestly be two different keys depending on what you listen for. So SpinDex analyses the track in several places and takes a vote, then:

- shows the key with a **dashed outline** when the model was not confident, meaning "check this by ear"
- offers the **runner-up keys** in the tooltip and as clickable suggestions in the tag editor
- **locks** any key you type in yourself, so re-analysing never overwrites your judgement

### Apollo — repairs compression damage (optional)

When audio is compressed to a small file (a 128 kbps MP3, a stream rip), the encoder permanently throws away parts of the sound. That is why such files can sound brittle, watery or smeared, especially on cymbals and vocals — and no amount of EQ brings it back, because the information is gone.

Apollo, from a 2024 research paper by JusperLee, was trained on pairs of the same music before and after compression, so it learned what the damage typically looks like and what tends to sit underneath it. Given a damaged track it produces a new version with a plausible reconstruction of what was lost.

**Be realistic about what it does.** Measured on a real damaged file, it audibly reduces compression artefacts, but it does **not** put back the missing high frequencies — a track cut off at 16 kHz is still cut off at 16 kHz afterwards, and SpinDex's quality score will not rise. Judge it by ear, on the copy it makes; the original is never touched unless you explicitly choose replace-with-backup. It takes roughly **30 minutes per track** on a normal CPU.

Apollo is optional and **not included** in SpinDex — its licence is share-alike, so redistributing it would attach those terms to SpinDex (see [Licensing](#licensing)). The setup script *is* included: run `setup-apollo.ps1` (it sits next to SpinDex after installing, or in `scripts/` in a checkout) and the Restore button appears once it finishes.

It is the one component with real prerequisites of its own: **Python 3.12 and git must be on your PATH**, because Apollo is Python model code rather than a single executable. The script builds it a private environment in `%APPDATA%\app.spindex.desktop\bin\apollo` — a few hundred MB of download, several minutes — and touches nothing else on your system.

### A helper that is not ML

- **ffmpeg** does the unglamorous work of turning any audio file into raw numbers the models can read, and writes the audio out again for Enhance, Steady tempo and Restore. Analysis cannot run without it.

## What SpinDex installs, and what you install yourself

SpinDex ships **only its own binaries and permissively-licensed files**. Anything under a copyleft licence is fetched by you, from its own publisher, so that nobody else's licence terms ride along with SpinDex.

| Component              | What it's for                                                                              | Ships in the installer?                                                                              |
| ---------------------- | ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------- |
| SpinDex + its analyzer | everything                                                                                 | Yes — ours, MIT                                                                                     |
| Beat This! model       | tempo detection                                                                            | Yes — MIT permits it                                                                                |
| OpenKeyScan model      | key detection                                                                              | Yes — MIT permits it                                                                                |
| **ffmpeg**       | decoding audio; required for all analysis, aiff playback, Enhance / Steady tempo / Restore | **No** — GPLv3. Run `install-components.ps1`                                                |
| **Apollo**       | ML restoration                                                                             | **No** — CC BY-SA 4.0. Run `setup-apollo.ps1` (ships with SpinDex; needs Python 3.12 + git) |

`install-components.ps1` is placed next to SpinDex when you install it, and downloads into `%APPDATA%\app.spindex.desktop\bin` — no administrator rights, and it survives app updates. Without any of these, SpinDex still scans, searches, plays mp3/flac, edits tags, finds duplicates and manages crates.

**One Windows prerequisite for analysis:** the analyzer uses ONNX Runtime, which needs the [Microsoft Visual C++ Redistributable (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe). Most machines already have it — installed by countless other applications — and `install-components.ps1` tells you if yours doesn't. It cannot be bundled into SpinDex: installing it requires administrator rights, and it cannot be linked in statically because the prebuilt ONNX Runtime is compiled against the dynamic runtime.

## Working with Serato

Serato is finicky about files, and SpinDex has been tuned against a real Serato library. Two rules matter:

1. **Close Serato before editing anything in SpinDex.** Serato holds files open and rewrites its crate files from memory when it exits, so edits made while it's running are silently lost. Every SpinDex dialog that touches a Serato file says so.
2. **Serato caches tags in its own database.** After editing, use *Rescan ID3 Tags* in Serato's Files panel — drag the tracks onto the button, since a whole-library rescan can skip external drives.

Serato's BPM and key columns come from its own analysis and override the file's tags, so pushing a key into a file will not repaint those columns for a track Serato has already analyzed.

If tags or covers you wrote with an older SpinDex build still don't appear, use **Update tags from SpinDex** on the crate (with Serato closed). Builds before 0.3.0 wrote WAV tags into an uppercase `ID3 ` chunk, which Serato does not read; the sweep rewrites only the files that need it.

## Tech stack

| Layer         | Technology                                             | What it does                                                                                                                                                                                                     |
| ------------- | ------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Desktop shell | [Tauri v2](https://v2.tauri.app/)                       | Wraps the web frontend in a native window (uses the OS's built-in WebView2 on Windows, so binaries stay small)                                                                                                   |
| Backend       | Rust                                                   | All the real work: filesystem scanning, tag reading ([lofty](https://crates.io/crates/lofty)), database ([rusqlite](https://crates.io/crates/rusqlite)), DSP and ML inference ([ort](https://crates.io/crates/ort)) |
| Frontend      | React + TypeScript, built with[Vite](https://vite.dev/) | The UI. It never touches files or the database directly                                                                                                                                                          |
| Storage       | SQLite (one file,`spindex.db` in the app data dir)   | Cache + search index only                                                                                                                                                                                        |

The frontend talks to the backend through **Tauri commands**: it calls `invoke("scan_library", ...)` (typed wrappers live in [src/api.ts](src/api.ts)), and Tauri routes that to a Rust function in [src-tauri/src/commands.rs](src-tauri/src/commands.rs). That's the only bridge between the two worlds.

## Getting started (development)

### 1. Prerequisites

- **Rust** 1.77+ — install via [rustup](https://rustup.rs/), then check with `rustc --version`
- **Node.js** 20+ — check with `node --version`
- **Microsoft C++ Build Tools** (Windows) — Rust needs a linker. Install "Desktop development with C++" via the [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) installer
- **WebView2** — preinstalled on Windows 10/11, nothing to do

You do *not* need the Tauri CLI globally — it runs through npm.

### 2. Install frontend dependencies

```sh
npm install
```

Rust dependencies are handled automatically by Cargo on first build.

### 3. Run the app in dev mode

```sh
npm run tauri dev
```

What this does, in order:

1. Starts the Vite dev server for the React frontend on `http://localhost:5173`
2. Compiles the Rust backend (the **first build takes several minutes** — Cargo compiles every dependency from source; later builds are cached)
3. Opens the SpinDex window pointed at the dev server

While it's running, frontend changes hot-reload instantly and Rust changes trigger a recompile and restart. `Ctrl+Shift+I` opens devtools inside the window.

The database is created at `%APPDATA%\app.spindex.desktop\spindex.db`. Deleting it is always safe — it just forces a full rescan.

### 4. Contributing changes

Implementation work happens in a **dedicated git worktree**, never the primary checkout — two sessions sharing one checkout share `HEAD` and the index, which has clobbered uncommitted edits before:

```sh
git worktree add .worktrees/<feature> -b feat/<feature>
# ...work, test, commit there...
git merge --ff-only feat/<feature> && git worktree remove .worktrees/<feature>
```

Note that `src-tauri/binaries/` is git-ignored, so a fresh worktree can't build the Tauri app until you link or copy it in.

## Other useful commands

| Command                                                              | What it does                                                                                                                                       |
| -------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `npm run tauri dev`                                                | Run the app in development mode (use this 99% of the time)                                                                                         |
| `npm run tauri build`                                              | Release build + Windows installer (NSIS, in`src-tauri/target/release/bundle/`) |
| `scripts\bump-version.ps1 <x.y.z>`                                 | Set the version in all three files that must agree (`-Check` verifies)                                                                           |
| `cargo test --lib` (inside `src-tauri/`)                         | The Rust unit suite — 286 tests, no sidecars or fixtures needed                                                                                   |
| `cargo run --bin spindex-scan -- <folder>` (inside `src-tauri/`) | Standalone CLI scanner — scans a folder into SQLite and prints timing stats, no GUI                                                               |
| `npx tsc --noEmit`                                                 | Type-check the frontend                                                                                                                            |
| `cargo test --test <name> -- --ignored` (inside `src-tauri/`)    | Env-gated live diagnostics against real files — see`src-tauri/tests/`                                                                           |
| `npm run dev`                                                      | Frontend only in a browser — mostly useless, because`invoke()` needs the Tauri shell                                                            |

## Project structure

```
spindex/
├── src/                      React frontend
│   ├── App.tsx               shared state, handlers, event subscriptions, modals
│   ├── columns.ts            column defs, sort state, localStorage loaders
│   ├── components/           Sidebar, Toolbar, TrackTable, deck, modals, dialogs, context menu
│   ├── viz/                  WebGL visualizer (engine, scenes, renderer)
│   ├── api.ts                typed invoke() wrappers, one per Tauri command
│   └── theme.ts              design tokens (colors, fonts), formatters
├── src-tauri/                Rust backend (the Tauri app)
│   ├── tauri.conf.json       window config, build hooks, sidecar bundling
│   ├── binaries/             sidecar exes + ML models (git-ignored; see scripts/)
│   ├── tests/                env-gated live diagnostics (need local test data)
│   └── src/
│       ├── commands.rs       Tauri command handlers (thin layer only)
│       ├── bin/              spindex-scan CLI, spindex-analyzer ML sidecar
│       └── core/
│           ├── scanner.rs    filesystem walking, change detection
│           ├── metadata.rs   tag reading/writing via lofty
│           ├── db.rs         SQLite setup, schema + migrations, queries
│           ├── analysis.rs   sidecar orchestration
│           ├── dsp.rs        mel spectrogram, beat picking, key vote
│           ├── quality.rs    LUFS, clipping, spectral cutoff, score
│           ├── enhance.rs    loudness enhancement + output machinery
│           ├── restore.rs    ML restoration (Apollo)
│           ├── tempo.rs      drift measurement + warp planning
│           ├── dupes.rs      duplicate detection + library similarity
│           ├── crate_order.rs  manual track ordering inside crates
│           ├── serato.rs     Serato crate parsing and writing
│           ├── rekordbox.rs  read-only Rekordbox export.pdb parsing
│           ├── devices.rs    USB drive watching
│           ├── copying.rs    copy/import helpers (never overwrite)
│           ├── enrich.rs     optional online metadata lookup
│           └── trash.rs      Recycle Bin deletion
├── scripts/                  sidecar fetch + model conversion scripts
└── website/                  the landing page (static HTML, published by .github/workflows/pages.yml)
```

## Website

`website/` is the landing page: plain static HTML, no build step. Open
`website/index.html` in a browser to preview it. Every push to `main` that
touches `website/` publishes it to GitHub Pages through
`.github/workflows/pages.yml` (repo setting: **Settings → Pages → Source →
GitHub Actions**). The download button points at a stable installer name,
`SpinDex-Setup-x64.exe`, so it never needs editing per release.

## Licensing

**SpinDex itself is licensed under the MIT License** (see [LICENSE](LICENSE)): you may use, copy, modify and redistribute it, as long as the copyright and licence notice stay with it. The licence covers the code, not the name: a modified version must not present itself as SpinDex.

That's a separate question from what SpinDex is *built from*. The rule for the installer is deliberately simple: **it contains only SpinDex's own binaries and permissively-licensed third-party files**, plus a few unmodified MPL-2.0 libraries. Nothing under the GPL, LGPL or CC BY-SA is redistributed, so none of those projects' terms attach to SpinDex — the only obligation the MIT/Apache/BSD components carry is keeping their attribution, which is what the table below and [THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt) do.

| Component                                       | Licence                                                                                                                                                     | Shipped?                                                                                                  |
| ----------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| SpinDex,`spindex-analyzer`                    | MIT                                                                                                                                                         | Yes                                                                                                       |
| Beat This! model                                | MIT — explicitly stated to cover the published weights, not just the code                                                                                  | Yes, with attribution                                                                                     |
| OpenKeyScan model                               | MIT (repo-wide licence file; no explicit statement about the weights specifically — see[THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt) for the caveat) | Yes, with attribution                                                                                     |
| ONNX Runtime, Tauri, React, rusqlite, lofty, … | MIT / Apache-2.0 / public domain                                                                                                                            | Compiled in                                                                                               |
| cssparser, selectors, dtoa-short, option-ext    | MPL-2.0 (used unmodified; source on crates.io)                                                                                                              | Compiled in                                                                                               |
| **ffmpeg**                                | **GPLv3** (`--enable-gpl --enable-version3`)                                                                                                        | **No** — fetched by `install-components.ps1` from [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) |
| **Apollo** (ML restoration)               | **CC BY-SA 4.0**                                                                                                                                      | **No** — fetched by `setup-apollo.ps1`; the Restore UI stays hidden until it exists              |

Because ffmpeg and Apollo are downloaded from their own publishers onto your machine and are never redistributed by this project, their licences apply to your copy of them and not to SpinDex. SpinDex invokes each as a separate process and links against none of them. Full details and source URLs are in [THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt).

The Windows installer is currently **unsigned**, so SmartScreen will warn on first run: *More info* → *Run anyway*.

### Trademarks

SpinDex is an independent project, not affiliated with or endorsed by Serato or AlphaTheta (Pioneer DJ). Serato is a trademark of Serato Limited; rekordbox is a trademark of AlphaTheta Corporation. These names are used only to say which file formats SpinDex works with.

## Troubleshooting

- **`link.exe not found` or linker errors** — the C++ Build Tools are missing; install them (see prerequisites)
- **First `npm run tauri dev` seems stuck** — it's compiling several hundred Rust crates; give it a few minutes and watch the terminal
- **Analyze is greyed out** — ffmpeg is missing (or, in a dev checkout, the ONNX models). Run `install-components.ps1`, then restart SpinDex
- **aiff files won't play, or Enhance / Steady tempo / Restore do nothing** — same cause: ffmpeg is not installed yet
- **Port 5173 already in use** — another Vite dev server is running; stop it
- **App window is blank** — the Vite server probably failed to start; check the terminal for frontend errors
- **Edits don't show in Serato** — see [Working with Serato](#working-with-serato) above
