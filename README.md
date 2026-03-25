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

## Building

### Prerequisites

- **Rust** 1.92+ (edition 2024)
- **GTK frontend**: `gtk4` and `libadwaita` development libraries
- **Qt frontend**: `qt6-base`, `qt6-declarative`, and `qt6-quickcontrols2` development libraries, plus `cmake`
- **Video support**: `ffmpeg` (runtime dependency)

#### Arch Linux

```bash
# GTK frontend
sudo pacman -S gtk4 libadwaita

# Qt frontend
sudo pacman -S qt6-base qt6-declarative qt6-quickcontrols2 cmake

# Video support
sudo pacman -S ffmpeg
```

#### Fedora

```bash
# GTK frontend
sudo dnf install gtk4-devel libadwaita-devel

# Qt frontend
sudo dnf install qt6-qtbase-devel qt6-qtdeclarative-devel cmake

# Video support
sudo dnf install ffmpeg
```

#### Ubuntu/Debian

```bash
# GTK frontend
sudo apt install libgtk-4-dev libadwaita-1-dev

# Qt frontend
sudo apt install qt6-base-dev qt6-declarative-dev cmake

# Video support
sudo apt install ffmpeg
```

### Build

```bash
# Build everything
cargo build --workspace --release

# Build only GTK frontend
cargo build -p traceless-gtk --release

# Build only Qt frontend
cargo build -p traceless-qt --release

# Build just the core library
cargo build -p traceless-core --release
```

## Usage

```bash
# Auto-detect desktop environment and launch appropriate frontend
./traceless

# Force a specific frontend
TRACELESS_FRONTEND=gtk ./traceless
TRACELESS_FRONTEND=qt ./traceless

# Launch a specific frontend directly
./traceless-gtk
./traceless-qt
```

## Architecture

```
traceless/
├── crates/
│   ├── core/           # Shared metadata reading/cleaning logic
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
