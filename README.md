# Traceless

A fast, safe metadata cleaner for Linux desktops, written in Rust.

Traceless lets you view and remove metadata from your files — EXIF data from photos, author info from documents, tags from audio files, and more. It provides native frontends for both GNOME (GTK4/libadwaita) and KDE Plasma (Qt6/QML), with automatic desktop environment detection at launch.

## Attribution

This project is inspired by [Metadata Cleaner](https://gitlab.com/rmnvgr/metadata-cleaner) by Romain Vigier and contributors. It has been rewritten from scratch in Rust to be faster, safer, and to eliminate the dependency on mat2 and Python.

## Supported Formats

| Format | Extensions | Engine |
|--------|-----------|--------|
| Images | `.jpg`, `.jpeg`, `.png`, `.webp` | little_exif + img-parts |
| PDF | `.pdf` | lopdf |
| Audio | `.mp3`, `.flac`, `.ogg`, `.wav`, `.m4a`, `.aac`, `.aiff` | lofty |
| Documents | `.odt`, `.ods`, `.odp`, `.docx`, `.xlsx`, `.pptx`, `.epub` | zip + quick-xml |
| Video | `.mp4`, `.mkv`, `.webm`, `.avi`, `.mov` | ffmpeg (CLI) |

## Features

- View all metadata in files before cleaning
- Full metadata stripping (removes all metadata)
- Lightweight cleaning mode (preserves data integrity, removes author/tool info)
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
2. Detect which toolkits (GTK4/Qt6) are available or can be installed
3. Let you choose which frontend(s) to build
4. Install required system packages (with your permission)
5. Install Rust via rustup if needed
6. Build and install the binaries to `~/.local/bin` (or `/usr/local/bin` with `--system`)

```bash
# Install to ~/.local/bin (default, no root needed for install step)
./install.sh

# Install system-wide to /usr/local/bin
./install.sh --system

# Non-interactive: build both frontends, install deps automatically
./install.sh --yes --all

# Build only the GTK frontend
./install.sh --gtk

# Build only the Qt frontend
./install.sh --qt
```

## Manual Build

### Prerequisites

- **Rust** 1.92+ (edition 2024)
- **GTK frontend**: GTK 4 and libadwaita development libraries
- **Qt frontend**: Qt 6 Base, Declarative, and QuickControls2 development libraries, plus CMake
- **Video support** (optional): `ffmpeg` at runtime

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
```
</details>

<details>
<summary><b>NixOS / Nix</b></summary>

```bash
nix-shell -p gtk4 libadwaita pkg-config cmake qt6.qtbase qt6.qtdeclarative rustup ffmpeg
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

# Enable GNOME 50 features (requires GTK >= 4.22, libadwaita >= 1.9)
cargo build -p traceless-gtk --release --features gnome_50
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
| XFCE, Cinnamon, MATE | GTK (libadwaita) | `XDG_CURRENT_DESKTOP` |
| Hyprland, Sway, i3, other WMs | GTK (default fallback) | Falls back to GTK |
| Any (manual override) | Either | `TRACELESS_FRONTEND=gtk\|qt` |

If only one frontend is installed, the launcher will use whichever is available regardless of desktop environment.

## Architecture

```
traceless/
├── crates/
│   ├── core/           # Shared metadata reading/cleaning logic (no UI deps)
│   ├── gtk-frontend/   # GTK4/libadwaita GNOME-native UI
│   ├── qt-frontend/    # Qt6/QML KDE-native UI
│   └── launcher/       # DE auto-detection + exec()
```

The core library is shared between both frontends. Each frontend is a separate binary with no dependency on the other's toolkit. The launcher binary detects the desktop environment via `XDG_CURRENT_DESKTOP` and `exec()`s the appropriate frontend.

## Testing

```bash
cargo test -p traceless-core
```

## License

GPL-3.0-or-later
