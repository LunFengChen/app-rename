# app-rename

Rust CLI for renaming Android package files to:

```text
包名_版本号.apk
包名_版本号.xapk
```

## Features

- Supports `.apk` and `.xapk`.
- APK metadata is read from `AndroidManifest.xml` inside the APK.
  - Supports normal Android binary XML manifests.
  - Also supports plain text XML manifests for test fixtures/unpacked-like zips.
- XAPK metadata is read from `manifest.json` first, then falls back to parsing embedded APK files.
- Collision-safe by default: if the destination exists, it writes `__1`, `__2`, ... instead of overwriting.
- Cross-platform release builds for Linux, macOS Intel, macOS Apple Silicon, and Windows.

## Install

Download a prebuilt binary from GitHub Releases, or build from source:

```bash
cargo install --path .
```

## Usage

```bash
# Rename one file in place
app-rename app.apk

# Rename all APK/XAPK files in a directory
app-rename /path/to/files

# Recursively scan a directory
app-rename -r /path/to/files

# Preview without changing files
app-rename -n app.apk

# Copy instead of rename
app-rename -c app.apk

# Overwrite existing destination
app-rename --overwrite app.apk
```

## Build

```bash
cargo build --release
```

Binary path:

```text
target/release/app-rename
```

## CI and release

GitHub Actions runs formatting, tests, and cross-platform release builds.

Create a release by pushing a version tag:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The release workflow uploads packaged binaries and SHA-256 checksum files.

## License

MIT
