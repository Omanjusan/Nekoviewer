# Changelog

All notable changes to this project will be documented in this file.

## [1.0.0] - 2026-07-04

### Added

- Added in-archive thumbnail display in the viewer
- Added corresponding viewer settings (can also be hidden)
- Added a filename search filter at the bottom of the explorer pane; supports glob-style wildcards (`*`, `?`); the checkbox toggles the filter on/off; the search text persists only for the current session and is not carried over to the next launch

### Fixed

- Fixed fullscreen paging on Wayland appearing to freeze (window stopped updating); now sets an AutoNoVsync flag to work around it
- Fixed settings dialog layout issues

## [0.9.0] - 2026-07-03

### Added

- Added 7z support (password-protected archives not implemented)

### Changed

- Reduced terminal output where possible; logs can now be viewed from the GUI

## [0.8.0] - 2026-07-02

### Added

- Added settings GUI

### Changed

- Animations now use a ring buffer, reducing memory usage

### Fixed

- Fixed animations playing back slower than intended

## [0.7.0] - 2026-06-30

### Added

- Multilingual support: language switcher menu placed on the right side of the menu bar in the thumbnail display area (ja/en/cn)
- (Primarily Windows) F5–F8 window position and size slots now support default slot numbers configurable in the config file

### Changed

- Cache optimizations

### Changed - Development Notes

- Core architecture refactored to MVC model
- Disabled eframe in multi-window setup due to update issues

### Known Issues

- Display issues when cache memory overflows
- Loading indicator needs clarification
- Config file internationalization pending

### In Progress

- Search functionality
- Page thumbnails and position marker within archives

## [0.6.1] - 2026-06-27

### Added
- `←` / `→` keys for file jump: jumps directly to page 1 of the previous/next file regardless of current page position

### Changed
- RAM cache is now managed in separate areas for vertical and horizontal scroll, per file; improves cache efficiency when browsing across files

## [0.5.0] - 2026-06-26

### Added
- Directory navigation now uses single-click (the [直] button has been removed)
- Invalid ZIP marker cache: ZIPs containing no images are marked under `.neko/invalid/`; the marker is automatically removed if the ZIP is updated
- Invalid ZIPs show a red X in the grid with a toast notification (auto-dismissed after 3 seconds) on double-click
- Silent skip of invalid ZIPs when navigating forward/backward in the viewer
- Thumbnail storage refactored from individual JPG files to a single database file

### Changed
- Directory tree expansion is now triggered by clicking ▶ only (single-click on the item navigates into the directory)

### Fixed
- Viewer no longer opens for ZIPs that contain no image entries; once cached as invalid, they are hidden from the explorer

### In Progress
- UI internationalization (i18n) support

## [0.4.0] - 2026-06-26

### Added
- File navigation via Shift+Scroll / Shift+↑↓: moves to the previous/next file when at the first/last page
- Linux local build support: added `setup.sh` and `Makefile` with guided dependency installation

### Fixed
- Linux build is now available again (removed from Known Issues)

### Changed
- README updated with clear first-time and update build instructions

## [0.3.1] - 2026-06-25

### Fixed
- GitHub Actions release now correctly includes the EXE file

## [0.3.0] - 2026-06-25

### Added
- AVIF format support (static images and animations)
- Animation support: GIF, AVIF, and WebP (APNG not supported)

### Fixed
- Cache collision between standalone files and archive files with the same name
- Cache is now regenerated when the source file is newer than the cached version

### Known Issues
- Middle mouse button click requires slight mouse movement to trigger fullscreen toggle
- Linux build is temporarily unavailable


## [0.2.0] - 2026-06-25

### Added
- Initial release
- GitHub Actions workflow for automated Windows build and release
