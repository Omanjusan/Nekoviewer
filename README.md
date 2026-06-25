# Nekoviewer

A single-binary desktop viewer for comfortably reading manga archives in ZIP / CBZ format.

[日本語版 README](README.ja.md)

---

## Purpose

Browse folders like a bookshelf, navigate into archives, and view their images — all within a single in-app viewer window.

- Linux / Windows support
- Direct filesystem access — no external database or service required
- Lightweight — thumbnail cache reduces redraw cost; Rust eliminates memory leaks and keeps it fast
- Single binary — no installation needed. Just the executable and its config file. Running it from a dedicated folder is recommended, especially when keeping the cache alongside the binary.
- No uninstaller needed — no registry writes. Delete the EXE and its auto-generated subfolders and you're clean.
- Network share (SMB) support — cache is stored locally , so paths with unusual network prefixes are handled correctly.

---

## Installation / Build

### Windows

Download the latest `nekoviewer.exe` from [GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) and place it in any folder. No installation required.

If your security software flags the file, see the [VirusTotal scan result](https://www.virustotal.com/gui/url/883c1d800c90c40c2ef478fbe8a2ad0627a8d780e3e7b825794864cb23c2b473) (as of v0.2.0).

### Linux

Requires the Rust toolchain (`cargo`).

```bash
git clone https://github.com/Omanjusan/Nekoviewer.git
cd Nekoviewer
cargo run --release
```

---

## Usage

### Launch

```
nekoviewer [folder path]
```

If no argument is given, the app starts from the folder set in `nekoviewer.conf` or the last opened folder.

### Controls

#### Main Window

| Action | Result |
|--------|--------|
| Click folder | Navigate into that folder's archive list |
| Double-click thumbnail | Open viewer window |
| Click sort header | Sort by filename / date / size |

#### Viewer Window

| Key | Action |
|-----|--------|
| `↓` / `Space` / Scroll down | Next page |
| `↑` / Scroll up | Previous page |
| `Home` | First page (not yet implemented) |
| `End` | Last page (not yet implemented) |
| `1` | Single page view |
| `2` | Spread view (left binding) |
| `3` | Spread view (right binding) |
| `4` | Spread offset −1 (clamped to −1–+1) |
| `5` | Spread offset +1 (clamped to −1–+1) |
| `F5`–`F8` | Save / restore window position and size slots |
| `Enter` | Original size |
| `Alt+Enter` | Toggle fullscreen / windowed |
| `Esc` | Close viewer window |

### Supported Formats

**Archives:** ZIP, CBZ (standalone image files are also supported)

**Images:** JPEG, PNG, WebP, GIF, BMP

**Animated playback:** WebP, GIF (APNG: TBD)

### Config File (`nekoviewer.conf`)

Auto-generated in the same folder as the executable on first launch.

```conf
[startup]
# Start from the last opened folder
use_last_dir = false
# Fixed startup folder (empty = home directory)
fixed_dir =

[cache]
# local : store under cache/ next to the executable
# xdg   : store under %LOCALAPPDATA%/nekoview/cache/
storage = local
# Max memory for page cache (MB). Default: 30% of system RAM
# max_mb =

[worker]
# Decoder thread count. 0 = auto (half of logical cores)
decode_threads = 0

[thumbnail]
# nearest / triangle / catmullrom / lanczos3
filter = triangle

[viewer]
# nearest / triangle / catmullrom / lanczos3
filter = catmullrom

[grid]
# Thumbnail long-edge size (px). Range: 64–512
thumb_size = 256
```

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
