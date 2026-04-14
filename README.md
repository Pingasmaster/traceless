# Traceless

![GPLv3](gplv3.png)

A fast, safe metadata cleaner for Linux desktops, written in Rust.

Traceless lets you view and remove metadata from your files: EXIF data from photos, author info from documents, tags from audio files, and more. It provides native frontends for both GNOME (GTK4/libadwaita) and KDE Plasma (Qt6/QML), with automatic desktop environment detection at launch.

## Attribution

Inspired by [Metadata Cleaner](https://gitlab.com/rmnvgr/metadata-cleaner) by Romain Vigier and contributors. Rewritten from scratch in Rust to be faster, safer, and free of any mat2/Python dependency.

## Supported Formats

| Category | Extensions |
|---|---|
| Images | `.jpg`, `.jpeg`, `.png`, `.webp`, `.tiff`, `.tif`, `.heic`, `.heif`, `.jxl`, `.gif`, `.bmp`, `.ppm`, `.pgm`, `.pbm`, `.pnm` |
| Documents | `.pdf`, `.docx`, `.xlsx`, `.pptx`, `.odt`, `.ods`, `.odp`, `.odg`, `.epub`, `.txt` |
| Audio | `.mp3`, `.flac`, `.ogg`, `.opus`, `.wav`, `.m4a`, `.aac`, `.aiff` |
| Video | `.mp4`, `.mkv`, `.webm`, `.avi`, `.mov`, `.wmv`, `.flv` |
| Markup | `.svg`, `.html`, `.htm`, `.xhtml`, `.css` |
| Archives | `.zip`, `.tar`, `.tar.gz`, `.tar.bz2`, `.tar.xz` |
| Other | `.torrent` |

## Features

- **Read** metadata from any supported file before cleaning.
- **Remove** all metadata thoroughly by default, no half-measures.
- **Recursive archive cleaning**: files inside ZIPs, TARs, and office documents are cleaned too, so an embedded JPEG inside a `.docx` will not leak camera EXIF or GPS.
- **Deterministic output**: cleaning the same file twice produces byte-identical results.
- **Optional sandboxing**: when [bubblewrap](https://github.com/containers/bubblewrap) is installed, `ffmpeg` runs inside a locked-down namespace with no filesystem access beyond the input and output paths.
- Drag-and-drop file adding, recursive folder scanning, concurrent processing.
- Automatic GNOME/KDE detection with manual override.

## Quick Install

The easiest way to build and install Traceless is with the included script:

```bash
./install.sh
```

It detects your distribution, installs any missing system packages, installs Rust via rustup if needed, builds the available frontends, and installs the binaries to `/usr/local/bin`.

```bash
./install.sh           # auto-detect, install to /usr/local/bin
./install.sh --user    # install to ~/.local/bin
./install.sh --all     # build both frontends
./install.sh --gtk     # GTK only
./install.sh --qt      # Qt only
./install.sh --ask     # confirm each step
```

## Manual Build

### Prerequisites

- **Rust** 1.92+ (edition 2024)
- **GTK frontend**: GTK 4 and libadwaita development libraries
- **Qt frontend**: Qt 6 Base, Declarative, and QuickControls2 development libraries, plus CMake
- **Video support** (optional): `ffmpeg` at runtime
- **Sandboxing** (optional, recommended): `bubblewrap` at runtime

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
cargo build --workspace --release        # everything
cargo build -p traceless-gtk --release   # GTK only
cargo build -p traceless-qt --release    # Qt only
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
traceless                          # auto-detect desktop environment and launch
TRACELESS_FRONTEND=gtk traceless   # force GTK
TRACELESS_FRONTEND=qt traceless    # force Qt
traceless-gtk                      # launch GTK directly
traceless-qt                       # launch Qt directly
```

The launcher picks GTK on GNOME-family desktops and Qt on KDE Plasma / LXQt, falling back to GTK on tiling WMs and unknown environments. If only one frontend is installed, it is used regardless.

## License

[GNU General Public License v3.0](LICENSE) or later.
