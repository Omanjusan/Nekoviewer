# Changelog

All notable changes to this project will be documented in this file.

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
