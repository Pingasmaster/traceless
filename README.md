# Traceless

![GPLv3](gplv3.png)

A fast, safe metadata cleaner for Linux desktops, written in Rust.

Traceless lets you view and remove metadata from your files: EXIF data from photos, author info from documents, tags from audio files, and more. It provides native frontends for both GNOME (GTK4/libadwaita) and KDE Plasma (Qt6/QML), with automatic desktop environment detection at launch.

## Attribution

This project is inspired by [Metadata Cleaner](https://gitlab.com/rmnvgr/metadata-cleaner) by Romain Vigier and contributors. It has been rewritten from scratch in Rust to be faster, safer, and to eliminate the dependency on mat2 and Python.

## Supported Formats

| Category | Extensions | Engine |
|---|---|---|
| Raster images | `.jpg`, `.jpeg`, `.png`, `.webp`, `.tiff`, `.tif`, `.heic`, `.heif`, `.jxl` | little_exif + img-parts |
| Animated / special | `.gif` | custom byte-level walker |
| Vector / markup | `.svg`, `.html`, `.htm`, `.xhtml`, `.css` | quick-xml + custom parsers |
| Harmless (no-op or trivial) | `.bmp`, `.ppm`, `.pgm`, `.pbm`, `.pnm`, `.txt` | built-in |
| PDF | `.pdf` | lopdf (object-graph strip) |
| Audio | `.mp3`, `.flac`, `.ogg`, `.opus`, `.wav`, `.m4a`, `.aac`, `.aiff` | lofty |
| Office documents | `.docx`, `.xlsx`, `.pptx`, `.odt`, `.ods`, `.odp`, `.odg`, `.epub` | zip + quick-xml (deep-clean) |
| Video | `.mp4`, `.mkv`, `.webm`, `.avi`, `.mov`, `.wmv`, `.flv` | ffmpeg (CLI, bitexact) |
| P2P | `.torrent` | custom bencode parser |
| Archives | `.zip`, `.tar`, `.tar.gz`, `.tar.bz2`, `.tar.xz` | tar + flate2 + bzip2 + xz2 |

## Features

### Metadata removal
- **Read** every recognized field before cleaning, including XMP/IPTC inside JPEGs
  and the XMP packet in PDF `/Metadata` streams.
- **Remove** all metadata by default. There is no separate "lightweight"
  mode — cleaning is always thorough.
- **Deep-clean office documents**: DOCX/XLSX/PPTX have `w:rsid*` / `w:nsid`
  fingerprints removed, tracked changes promoted or dropped, comment ranges
  unanchored, `[Content_Types].xml` and `document.xml.rels` rewritten, and
  junk files (`customXml/`, printer settings, theme, `viewProps`, `presProps`,
  comments, `docProps/custom.xml`, …) omitted entirely. ODT/ODS/ODP drop
  `Thumbnails/`, `Configurations2/`, and `layout-cache`, and strip
  `<text:tracked-changes>`. EPUB metadata blocks are regenerated with a fresh
  v4 UUID; `iTunesMetadata.plist` and `META-INF/calibre_bookmarks.txt` are
  dropped; archives containing `META-INF/encryption.xml` are refused.
- **Recursive archive cleaning**: every known-format member inside a ZIP, TAR,
  or office archive is itself cleaned — an embedded JPEG inside a `.docx`
  no longer leaks camera EXIF / GPS.
- **Deterministic output**: ZIP member timestamps are pinned to 1980-01-01,
  comments cleared, and entries sorted lexicographically (with `mimetype`
  first for ODF/EPUB). TAR members have uid/gid/mtime/uname/gname zeroed.
  Two cleanings of the same input produce byte-identical output.
- **PDF object-graph strip**: `/Info`, `/Metadata`, `/OpenAction`, `/AA`,
  `/Names/EmbeddedFiles`, `/Names/JavaScript`, `/AcroForm`, `/StructTreeRoot`,
  `/MarkInfo`, `/PieceInfo`, `/PageLabels`, `/Outlines`, `/Perms`, per-page
  `/Annots`, per-page `/Metadata`, and the trailer `/ID` are all removed.
  Image XObject metadata streams are stripped too.
- **Video bitexact mode**: ffmpeg is invoked with `-fflags +bitexact
  -flags:v +bitexact -flags:a +bitexact -disposition 0` so the output
  container has no `encoder=Lavf…` fingerprint.

### Safety
- **TAR safety checks**: setuid / setgid, absolute paths, `..` path
  traversal, symlinks escaping the archive, hardlinks, device files, and
  duplicate member names are all refused up front.
- **Optional sandboxing**: when [bubblewrap](https://github.com/containers/bubblewrap)
  is installed, ffmpeg / ffprobe are executed inside a fresh namespace
  with `--unshare-all --die-with-parent --clearenv`, a read-only `/usr`
  bind, a tmpfs `/tmp`, and tight RO/RW binds for just the input and
  output paths. Falls back to unsandboxed exec when bwrap is absent.
- **Unknown-member policy** (`UnknownMemberPolicy::{Keep, Omit, Abort}`):
  library consumers can choose whether unrecognized members of an
  archive are kept verbatim (default), silently dropped, or rejected
  outright with an error.

### UI
- Drag-and-drop file adding
- Recursive folder scanning
- Concurrent file processing
- Automatic GNOME/KDE detection with manual override

## Quick Install

The easiest way to build and install Traceless is with the included script:

```bash
./install.sh
```

The script will:
1. Detect your Linux distribution
2. Detect which toolkits (GTK4/Qt6) are available
3. Automatically install missing system packages
4. Install Rust via rustup if needed
5. Build frontends for all detected toolkits
6. Install binaries to `/usr/local/bin`

If no toolkit is detected, it asks which to install.

```bash
# Auto-detect, build, and install to /usr/local/bin
./install.sh

# Install to ~/.local/bin instead
./install.sh --user

# Force build both frontends (installs missing toolkit deps)
./install.sh --all

# Build only the GTK frontend
./install.sh --gtk

# Build only the Qt frontend
./install.sh --qt

# Interactive mode: confirm each step
./install.sh --ask
```

## Manual Build

### Prerequisites

- **Rust** 1.92+ (edition 2024)
- **GTK frontend**: GTK 4 and libadwaita development libraries
- **Qt frontend**: Qt 6 Base, Declarative, and QuickControls2 development libraries, plus CMake
- **Video support** (optional): `ffmpeg` at runtime
- **Sandboxing** (optional, recommended): `bubblewrap` at runtime — when
  present, ffmpeg / ffprobe invocations are executed under a namespace
  sandbox with no filesystem access beyond the input and output paths.

### Install dependencies by distro

<details>
<summary><b>Arch Linux / Manjaro</b></summary>

```bash
# GTK frontend
sudo pacman -S gtk4 libadwaita

# Qt frontend
sudo pacman -S qt6-base qt6-declarative qt6-quickcontrols2 cmake

# Video support (optional)
sudo pacman -S ffmpeg

# Sandboxing (optional, recommended)
sudo pacman -S bubblewrap
```
</details>

<details>
<summary><b>Fedora</b></summary>

```bash
# GTK frontend
sudo dnf install gtk4-devel libadwaita-devel

# Qt frontend
sudo dnf install qt6-qtbase-devel qt6-qtdeclarative-devel cmake

# Video support (optional)
sudo dnf install ffmpeg-free  # or ffmpeg from RPM Fusion

# Sandboxing (optional, recommended)
sudo dnf install bubblewrap
```
</details>

<details>
<summary><b>Ubuntu / Debian / Linux Mint / Pop!_OS</b></summary>

```bash
# GTK frontend
sudo apt install libgtk-4-dev libadwaita-1-dev

# Qt frontend
sudo apt install qt6-base-dev qt6-declarative-dev qml6-module-qtquick-controls cmake

# Video support (optional)
sudo apt install ffmpeg

# Sandboxing (optional, recommended)
sudo apt install bubblewrap
```
</details>

<details>
<summary><b>openSUSE</b></summary>

```bash
# GTK frontend
sudo zypper install gtk4-devel libadwaita-devel

# Qt frontend
sudo zypper install qt6-base-devel qt6-declarative-devel qt6-quickcontrols2-devel cmake

# Video support (optional)
sudo zypper install ffmpeg

# Sandboxing (optional, recommended)
sudo zypper install bubblewrap
```
</details>

<details>
<summary><b>Void Linux</b></summary>

```bash
# GTK frontend
sudo xbps-install gtk4-devel libadwaita-devel

# Qt frontend
sudo xbps-install qt6-base-devel qt6-declarative-devel cmake

# Video support (optional)
sudo xbps-install ffmpeg

# Sandboxing (optional, recommended)
sudo xbps-install bubblewrap
```
</details>

<details>
<summary><b>Alpine Linux</b></summary>

```bash
# GTK frontend
sudo apk add gtk4.0-dev libadwaita-dev

# Qt frontend
sudo apk add qt6-qtbase-dev qt6-qtdeclarative-dev cmake

# Video support (optional)
sudo apk add ffmpeg

# Sandboxing (optional, recommended)
sudo apk add bubblewrap
```
</details>

<details>
<summary><b>NixOS / Nix</b></summary>

```bash
nix-shell -p gtk4 libadwaita pkg-config cmake qt6.qtbase qt6.qtdeclarative rustup ffmpeg bubblewrap
```
</details>

### Build

```bash
# Build everything
cargo build --workspace --release

# Build only GTK frontend
cargo build -p traceless-gtk --release

# Build only Qt frontend
cargo build -p traceless-qt --release
```

### Install manually

```bash
# To ~/.local/bin
install -Dm755 target/release/traceless     ~/.local/bin/traceless
install -Dm755 target/release/traceless-gtk ~/.local/bin/traceless-gtk
install -Dm755 target/release/traceless-qt  ~/.local/bin/traceless-qt

# Or system-wide
sudo install -Dm755 target/release/traceless     /usr/local/bin/traceless
sudo install -Dm755 target/release/traceless-gtk /usr/local/bin/traceless-gtk
sudo install -Dm755 target/release/traceless-qt  /usr/local/bin/traceless-qt
```

## Usage

```bash
# Auto-detect desktop environment and launch appropriate frontend
traceless

# Force a specific frontend
TRACELESS_FRONTEND=gtk traceless
TRACELESS_FRONTEND=qt traceless

# Launch a specific frontend directly
traceless-gtk
traceless-qt
```

### Which frontend is used where?

| Desktop Environment | Frontend | Detection |
|-------------------|----------|-----------|
| GNOME, Unity, Budgie, Pantheon, COSMIC | GTK (libadwaita) | `XDG_CURRENT_DESKTOP` |
| KDE Plasma, LXQt | Qt (QML) | `XDG_CURRENT_DESKTOP` |
| XFCE, Cinnamon, MATE, Deepin, Enlightenment | GTK (libadwaita) | `XDG_CURRENT_DESKTOP` |
| Hyprland, Sway, i3, other WMs | GTK (default fallback) | Falls back to GTK |
| Any (manual override) | Either | `TRACELESS_FRONTEND=gtk\|qt` |

If only one frontend is installed, the launcher will use whichever is available regardless of desktop environment.

## Architecture

```
traceless/
├── crates/
│   ├── core/           # Shared metadata reading/cleaning logic (no UI deps)
│   │   └── src/handlers/
│   │       ├── image.rs       # JPEG, PNG, WebP, TIFF, HEIC/HEIF, JXL
│   │       ├── gif.rs         # GIF comment / application-ext walker
│   │       ├── svg.rs         # quick-xml RDF / Inkscape / sodipodi strip
│   │       ├── pdf.rs         # lopdf object-graph strip
│   │       ├── audio.rs       # lofty tag removal + FLAC picture recursion
│   │       ├── video.rs       # ffmpeg -bitexact bridge
│   │       ├── document.rs    # OOXML / ODF / EPUB dispatcher
│   │       ├── ooxml.rs       # DOCX / XLSX / PPTX deep clean
│   │       ├── odf.rs         # ODT / ODS / ODP deep clean
│   │       ├── epub.rs        # EPUB metadata regen + DRM refusal
│   │       ├── archive.rs     # Plain ZIP / TAR (+ gz/bz2/xz)
│   │       ├── harmless.rs    # text/plain, BMP, PPM copy
│   │       ├── html.rs        # Tag-level meta / title stripper
│   │       ├── css.rs         # /* */ comment stripper
│   │       ├── torrent.rs     # Bencode allowlist
│   │       ├── xmp.rs         # XMP + IPTC IIM field parsers
│   │       ├── sandbox.rs     # bubblewrap wrapper for external tools
│   │       ├── xml_util.rs    # Shared XML attribute-sort helper
│   │       └── zip_util.rs    # Shared ZIP member normalization
│   ├── gtk-frontend/   # GTK4/libadwaita GNOME-native UI
│   ├── qt-frontend/    # Qt6/QML KDE-native UI
│   └── launcher/       # DE auto-detection + exec()
```

The core library is shared between both frontends. Each frontend is a separate binary with no dependency on the other's toolkit. The launcher binary detects the desktop environment via `XDG_CURRENT_DESKTOP` and `exec()`s the appropriate frontend.

## Testing

The core library ships with an extensive test suite that mirrors mat2's
upstream tests format-for-format. Run it with:

```bash
cargo test -p traceless-core
```

At the time of writing: **114 unit tests + 84 integration tests**, all
passing. The integration tests build synthetic "dirty" fixtures on the
fly (via the crates the cleaner already depends on, and optionally
ffmpeg for audio/video), and assert round-trip cleanliness, determinism,
and idempotence for every supported format.

### Strict lint gate

The workspace is configured to deny `clippy::pedantic` across every
crate. The full lint gate is:

```bash
cargo clippy --workspace --all-targets -- \
    -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo
```

New code must pass this cleanly before being committed.

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE) or later.
