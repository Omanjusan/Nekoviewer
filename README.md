# Nekoviewer

A single-binary desktop viewer for comfortably reading manga archives in ZIP / CBZ / TAR / 7z format.

[日本語版 README](README.ja.md)

---

## Purpose

- Browse folders like a bookshelf, navigate into archives, and view their images — all within a single in-app viewer window.
- A viewer for people who collect and organize archives.
- The author built a viewer that fits their own needs.
- An experiment in how far AI coding can go.

## Features

- Linux / Windows support
- Direct filesystem access — no dependency on external services or servers
- Archive scoring — when you read to the end of an archive, a rating overlay appears automatically so you can give a score from ★0.5 to ★5. Close it without doing anything and the archive stays unrated.
- Sorting by view count, which is recorded automatically. It can be combined with the main sort keys (filename, date, file size) for compound sorting, and you can switch between view-count order and score order. For example, you can keep "highest score first" while also viewing larger files first within the same score.
- Network share (SMB) support — cache is stored locally, so it keeps working even with unusual network paths.
- Animated GIF, WebP, and AVIF playback — handles large animation files, which is especially useful for high-resolution AI-generated animations.
- Favorite file support — a single flag can apply to many favorite folders at once.
- Limited search — search files that already have cached thumbnails.
- Per-archive spread mode setting — saved automatically and restored on reopen.
- Optional per-archive sort settings — restored on reopen and updated when leaving the viewer.
- Focused purely on viewing, without file-manager operations such as move, delete, or copy, reducing the risk of accidental changes. (Copy support is planned.)
- Multilingual support (ja/en/cn)
- No ads, no telemetry

Demo GIF
<p align="center">
  <img width="500" height="298" alt="Image" src="https://github.com/user-attachments/assets/6b986484-74ce-47cf-82d0-f5a329be600a" />
</p>

---

## Installation

### Windows

Download the latest `nekoviewer.exe` from [GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) and place it in any folder. No installation required, but running it from a dedicated folder is recommended.

### Linux

An AppImage and a Flatpak file are distributed on [GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) (not through Flathub or other official repositories). For the most reliable result, build from source as described in [Build](#build) below.

#### Flatpak

```bash
flatpak install --user ./Nekoviewer-*-x86_64.flatpak
flatpak run io.github.Omanjusan.Nekoviewer
```

The Flatpak can browse home folders and mounted drives read-only. NekoViewer writes only its private settings and cache data under `~/.var/app/io.github.Omanjusan.Nekoviewer/`.

#### AppImage

Download `Nekoviewer-*-x86_64.AppImage` from [GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest), make it executable, and run it.

```bash
chmod +x ./Nekoviewer-*-x86_64.AppImage
./Nekoviewer-*-x86_64.AppImage
```

---

## Usage

### Note for Windows

Windows SmartScreen may show a warning. Click "More info" and then "Run anyway" to launch the app. This happens on every release.

### Launch

```
Windows: nekoviewer.exe
Linux: nekoviewer
```

A [folder path] argument is accepted, but in general, running it without arguments is fine.

### Recommended Setup

- Key assignment is supported. The recommended setup is as follows:
  1. Turn the "Toolbox" button in the explorer's menu bar ON.
  2. Open any image or archive, then right-click a cell in the toolbox shown in the viewer window and turn each function you need into a button.
  3. Right-click a button cell again and choose "Assign key".

  With this setup you can, for example, use the WASD keys for next/previous page and next/previous file.
- A viewer position & size lock is available. On a high-resolution monitor, it is recommended to keep the viewer fixed in place (not supported on Wayland).
- The app always keeps the explorer and the viewer in a one-to-one relationship. Even if you reopen a file, as long as the viewer exists, its layout is preserved and your window arrangement on the desktop is not disturbed.

### Supported Formats

**Archives:** ZIP, CBZ, 7Z, CB7, TAR, CBT, tar.gz/tgz, tar.zst/tzst (standalone image files are also supported)
(tar.xz not supported yet, RAR under consideration. See [docs/formats.md](docs/formats.md) for details)

**Images:** JPEG, PNG, WebP, GIF, BMP, AVIF, TIFF

**Animated playback:** AVIF, WebP, GIF (APNG: TBD)

## Build

### First time (source build)

Building from source requires the Rust toolchain (`cargo`) and `make`.

```bash
git clone https://github.com/Omanjusan/Nekoviewer.git
cd Nekoviewer
make release
./target/release/nekoviewer
```

`make release` will guide you through installing any missing dependencies (e.g. `nasm`, `dav1d`) on first run.

### Updating on Linux

```bash
git pull
make release
./target/release/nekoviewer
```

If you're not sure what to do, run `make help` to show the help.

### For developers: Flatpak development build

Most users can skip this section. Install the official Flathub Builder and the Rust SDK extension, then run `make flatpak`:

```bash
flatpak install --user flathub org.flatpak.Builder org.freedesktop.Sdk.Extension.rust-stable//25.08
make flatpak
```

---

## Contributing

Bug reports and feature requests are welcome as Issues. For the policy on pull requests and more, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Security Policy

For details on malware scanning and how to report a problem, see [SECURITY.md](SECURITY.md).

## Privacy Policy

This app does not collect any user data. Generated thumbnails, view counts, scores, and similar data are stored only in a local database and used only within the app's own features. There is no telemetry of any kind. Only the translation feature uses network communication, to talk to a local LLM; no communication takes place unless the user enters a URL in the settings.

## AI Assistance

This project is developed with the support of **Claude (Anthropic)** as an AI assistant.

Claude is used for design discussions, code review, and refactoring suggestions. All final decisions are made by the human author.

---

## License

MIT License — see [LICENSE](LICENSE) for the full text.

---

## Third-Party Licenses

For the licenses of the third-party libraries used by this software (Rust crates, and statically linked native libraries such as dav1d, libavif, and libwebp), see [THIRDPARTYNOTICES.md](THIRDPARTYNOTICES.md).
