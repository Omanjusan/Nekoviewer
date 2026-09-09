# Changelog

All notable changes to this project will be documented in this file.

## [1.7.0] - 2026-09-09

### Added

- Added indicators to the upper-right corner of thumbnails showing the saved state of three settings: binding direction (`L` or `R`), sort settings (`S`), and custom archive thumbnail (`T`)

### Fixed

- Reduced flickering during loading and resizing
- Added the ability to save and restore the sort order for each archive, accessible from the viewer's right-click menu
- Added the ability to select any page as an archive's thumbnail, accessible from the viewer's right-click menu
- Synchronized the folder tree with the folder restored at application startup
- Fixed the favorite and thumbnail-load-failure indicators overlapping
- Fixed combinations of virtual pages appearing at the beginning and end of an archive when adjusting the spread offset. The offset controls are now disabled when they would cause destructive page movement
- Fixed favorites and search writing to the database in a way that could compromise data consistency
- Fixed incorrect page transition animations when using the offset controls

## [1.6.0] - 2026-08-22

### Added

- Re-added a Linux AppImage to Releases
- Folder navigation in the item pane now reflects in the folder tree pane
- Added mouse drag-and-drop scrolling to the thumbnail item area

#### Added file search limited to thumbnail-cached data

- Added a Search tab to the file explorer. The search feature is available from this tab.
- To avoid overly complex results from a global search, searches are limited to the current folder, with an option to include its subdirectories. Only files with cached thumbnails within that scope are searched.

##### How to search

1. Select the Search tab.
2. The pane layout changes; use the drive list and folder tree to choose the base folder to search.
3. Set the search criteria in the leftmost pane.
4. Press the Start Search button.
5. Matching items appear in the area on the right.

##### Search limitations

- This is not a general-purpose search like `find` or Windows Search.
- Newly created image directories cannot be searched at all until at least one file has a cached thumbnail, which makes the directory recognizable as a search target.
- Files without thumbnails are not added to the thumbnail cache during a search.
- Search history is reset for each application session.

### Fixed

- Fixed the mouse cursor icon not rendering correctly in the Flatpak build

### Changed

- XDG/user-directory storage is now the official location for settings. If settings are still stored next to the executable, a migration choice is shown at startup. Leftover files must be removed manually.

## [1.5.1] - 2026-08-16

### Changed

- Replaced the failed Linux AppImage distribution path with a Flatpak package using the Freedesktop runtime.
- Enforced read-only access to user content in the Flatpak sandbox while keeping settings and caches writable in the app-private XDG directories.
- Defaulted console logging to off, viewer resizing to Lanczos3, and image decoding to follow the viewer window size.

## [1.5.0] - 2026-08-16

### Added

- Added a Linux AppImage to Releases

### Changed

- Config/state storage now defaults to XDG locations (`%APPDATA%` on Windows). A `nekoview` folder is created there to hold the config files.
- Added a migration feature for switching away from binary-adjacent storage. After migration, only the old config file is deleted; other leftover files must be removed manually.

## [1.4.4] - 2026-08-07

### Fixed

- Fixed text input being blocked in the filter field
- Fixed moiré and blurring caused by stale decoded images being displayed

## [1.4.2]

### Changed

- Promoted the translation feature from experimental status

### Fixed

- Fixed the favorite creation dialog not working correctly

## [1.4.0] - 2026-07-13

### Added

- Experimental: added OCR-to-translation feature via connection to a local LLM
- Added key assignment feature (work in progress, not yet complete)

### Fixed

- Fixed settings items not being saved/restored correctly
- Fixed thumbnails being stretched to match the display frame's aspect ratio instead of preserving the original aspect ratio

## [1.3.0] - 2026-07-09

### Added

- Added support for folder items inside the thumbnail grid
- Added full keyboard operation support
  Tab / Shift+Tab moves focus between explorer panes, and arrow keys, Enter, and Esc cover most of the main operations from there
  (submenu access is not supported)

### Fixed

- Fixed a delay when accessing network drives, especially over SMB

## [1.2.0] - 2026-07-07

### Added

#### Added favorites feature

- Added a new tab at the top of the folder tree for favorites, giving access to favorite folders from there
- Favorite items can be set via right-click menu in both the explorer and viewer panes; in the explorer, for a single file the item directly under the mouse hover position is targeted, while with multiple selections the hover position is ignored and the selected items are targeted instead
- A single file can be linked to multiple favorite folders, allowing simultaneous use of folders organized by author, pure favorites, etc.
- Caution is needed when mixing items from a network drive with items from local storage

### Changed

- Prevented multiple instances of the app from launching

### Fixed

- Fixed incorrect playback speed for animation files inside archives
- Fixed the scrollbar not appearing in the settings screen, making the close/apply buttons visually unreachable

## [1.1.0] - 2026-07-05

### Added

#### HOME / END key support

- In the explorer, moves the thumbnail selector to the first/last item
- In the viewer, moves to the first/last position within the archive

#### Save current spread settings from the viewer's right-click menu

- Single-page mode cannot be saved, but it can still be selected from the menu when overwriting a save; selecting it just resets to the default (no-save) value
- Binding direction and offset value are saved as well
- Page position is not saved; state is restored at the start of the file. Restoration preserves the offset value without breaking it, so a virtual page for position correction may appear — this is not a bug
- The database file is placed next to the executable; delete it to reset

#### Added page numbers to the thumbnail bar

#### Additional archive format support

- Added TAR / CBT support (uncompressed and gzip-compressed): `.tar` / `.cbt` / `.tgz` / `.tar.gz`
- Added TAR + zstd compression support (`.tar.zst` / `.tzst`); pure Rust implementation, no added C dependency

#### Additional image format support

- Added TIFF / TIF support (pure Rust implementation); multi-page TIFF only displays the first page; some compression schemes (CCITT G3/G4, JPEG-in-TIFF, etc.) are not supported

### Fixed

- File type detection now inspects the leading binary bytes of the file instead of relying solely on the extension when decoding/extracting; this also fixes a fallback failure bug encountered with files that couldn't be decoded
- Fixed heavy rendering performance in the thumbnail bar

### Under Consideration

- RAR archive format support
- Favorites folder support: a spec for collecting favorited files into a virtual favorites folder

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
