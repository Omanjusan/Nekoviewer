# Nekoviewer

A single-binary desktop viewer for comfortably reading manga archives in ZIP / CBZ format.

[日本語版 README](README.ja.md)

---

## Purpose

- Browse folders like a bookshelf, navigate into archives, and view their images — all within a single in-app viewer window.
- A viewer for people who collect and organize archives.
- An experiment in how far AI coding can go.

- Linux / Windows support
- Direct filesystem access — no external database or service required
- Lightweight — thumbnail cache reduces redraw cost; Rust eliminates memory leaks and keeps it fast
- Single binary — no installation needed. Just the executable and its config file. Running it from a dedicated folder is recommended, especially when keeping the cache alongside the binary.
- No uninstaller needed — no registry writes. Delete the EXE and its auto-generated subfolders and you're clean.
- Network share (SMB) support — cache is stored locally, so it keeps working even with unusual network paths.
- Animated GIF, WebP, and AVIF playback — ring-buffer playback supports a wide range of file sizes, even large animations.
- Favorite file support — a single flag can apply to many favorite folders at once.
- Per-archive spread mode setting — saved automatically and restored on reopen.
- Multilingual support (ja/en/cn)
- No ads, no telemetry

Demo GIF
<p align="center">
  <img width="600" alt="Nekoviewer demo" src="https://github.com/user-attachments/assets/e1320011-5cba-4e2c-91ec-5526d8b0f9c2" />
</p>

---

## Installation / Build

### Windows

Download the latest `nekoviewer.exe` from [GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) and place it in any folder. No installation required, but running it from a dedicated folder is recommended.

### Linux


Requires the Rust toolchain (`cargo`) and `make`.

#### First time

```bash
git clone https://github.com/Omanjusan/Nekoviewer.git
cd Nekoviewer
make release
./target/release/nekoviewer
```

`make release` will guide you through installing any missing dependencies (e.g. `nasm`, `dav1d`) on first run.

#### Updating

```bash
git pull
make release
./target/release/nekoviewer
```

Run `make help` if you're not sure what to do.

---

## Usage

### Updating on Windows

Windows SmartScreen may block the app from launching — this is not a bug. Click "More info" and then "Run anyway" to launch it. This happens on every release.

### Launch

```
Windows: nekoviewer.exe
Linux: nekoviewer
```

A [folder path] argument is accepted, but in general, running it without arguments is fine.

### Controls

Key assignment is supported; the defaults are listed below.

#### Main Window

| Action | Result |
|--------|--------|
| Click folder | Navigate into that folder's archive list |
| Enter / Double-click thumbnail | Open the file at the selector position in the viewer window |
| Click sort header | Sort by filename / date / size |
| Arrow keys / Click thumbnail | Move the item selector |

The selector is shown in blue for archive files and red for standalone image files.

#### Viewer Window

| Key | Action |
|-----|--------|
| `↓` / `Space` / Scroll down | Next page |
| `↑` / Scroll up | Previous page |
| `SHIFT+↓` / `SHIFT+Scroll down` | Move to next file (when at last page) |
| `SHIFT+↑` / `SHIFT+Scroll up` | Move to previous file (when at first page) |
| `←` | Next file (jump to page 1, regardless of current page) |
| `→` | Previous file (jump to page 1, regardless of current page) |
| `Home` | First page |
| `End` | Last page |
| `1` | Single page view |
| `2` | Spread view (left binding) |
| `3` | Spread view (right binding) |
| `4` | Spread offset −1 (clamped to −1–+1) |
| `5` | Spread offset +1 (clamped to −1–+1) |
| `F5`–`F8` | Save / restore window position and size slots (doesn't work on wayland) |
| `Enter` / Left double-click | Toggle between original size and fit to window |
| `Alt+Enter` / Middle mouse button | Toggle fullscreen / windowed |
| `Esc` | Close viewer window |

### Supported Formats

**Archives:** ZIP, CBZ, 7Z, CB7, TAR, CBT, tar.gz/tgz, tar.zst/tzst (standalone image files are also supported)
(tar.xz not supported yet, RAR under consideration. See [docs/formats.md](docs/formats.md) for details)

**Images:** JPEG, PNG, WebP, GIF, BMP, AVIF, TIFF

**Animated playback:** AVIF, WebP, GIF (APNG: TBD)

### Config File (`nekoviewer.conf`)

Most settings have moved to the in-app GUI settings dialog. The config file is now only used for initial/advanced setup; see the settings dialog for day-to-day configuration.

---

## AI Assistance

This project is developed with the support of **Claude (Anthropic)** as an AI assistant.

Claude is used for design discussions, code review, and refactoring suggestions. All final decisions are made by the human author.

---

## License

MIT License

Copyright (c) 2025 Omanjusan

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

---

## Third-Party Licenses

This software uses the following third-party libraries:

- **[redb](https://github.com/cberner/redb)** — Embedded key-value database used for thumbnail disk cache. Licensed under MIT OR Apache-2.0.
