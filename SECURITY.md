[← Back to README](README.md)

# Security Policy

## Malware Scanning

Since v1.9.0, every file in each release has been scanned for malware before distribution. The scan is also meant to let the author confirm that the distributed files are actually harmless. Scanning is done with Hybrid Analysis. Releases are published only when the scan does not flag anything.

## How to Report a Problem

If you find a security problem or suspicious behavior, please report it on [GitHub Issues](https://github.com/Omanjusan/Nekoviewer/issues). Any language is fine, since reports are run through translation.

The following information helps with investigation:

- Nekoviewer version and OS (Windows / Linux)
- Distribution type (exe / Flatpak / AppImage / source build)
- What the problem is, and how to reproduce it
- If possible, logs or screenshots

## Response Policy

- I will respond as best I can, but on a best-effort basis. No response time or outcome is guaranteed.
- Security-related pull requests are accepted, but they are not necessarily merged.

## What to Report

If you notice any of the following, it may be a security problem. Please report it.

- **Unintended network traffic:** The app connects to an external host even though no translation URL is configured.
- **Unintended file changes:** Archive or image files are modified, moved, or deleted. As of v1.10, this app is designed to be view-only.
- **Crashes or exploitable behavior:** Opening a particular archive or image file causes a crash, abnormal memory usage, or unexpected behavior.
- **Detection of the release files:** Antivirus software flags an official release file as malware.
- **Settings or data written elsewhere:** Settings or cache are written somewhere other than the `nekoview` / `nekoviewer` folder in the user directory.

Ordinary bugs, such as display glitches or usability complaints, can be reported in the same way.

### Notes When Reporting

- When attaching files to reproduce a problem, please use files that contain no copyrighted material or personal information. If you can create a minimal file that still reproduces the problem, please attach that.
