#!/usr/bin/env bash
#
# Traceless install script
# Detects distro & toolkits, installs deps, builds, and installs binaries.
#
set -euo pipefail

# ── Defaults ─────────────────────────────────────────────────────────────────

PREFIX="/usr/local"
AUTO_YES=true
WANT_GTK=auto
WANT_QT=auto
WANT_ALL=false

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

# ── Parse args ───────────────────────────────────────────────────────────────

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Options:
  --user        Install to ~/.local instead of /usr/local
  --prefix DIR  Install to a custom prefix
  --gtk         Build only the GTK frontend
  --qt          Build only the Qt frontend
  --all         Build both frontends
  --ask         Interactive mode: ask before each step (default is auto)
  --help, -h    Show this help

Examples:
  $0                # Auto-detect toolkits, install deps, build & install
  $0 --all          # Force build both frontends (install missing deps)
  $0 --gtk          # Build GTK frontend only
  $0 --user         # Install to ~/.local/bin instead of /usr/local/bin
  $0 --ask          # Interactive mode: confirm each step
EOF
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --user)     PREFIX="$HOME/.local"; shift ;;
        --system)   PREFIX="/usr/local"; shift ;;
        --prefix)   PREFIX="$2"; shift 2 ;;
        --gtk)      WANT_GTK=yes; WANT_QT=no; shift ;;
        --qt)       WANT_QT=yes; WANT_GTK=no; shift ;;
        --all)      WANT_ALL=true; shift ;;
        --ask)      AUTO_YES=false; shift ;;
        --yes|-y)   AUTO_YES=true; shift ;;
        --help|-h)  usage ;;
        *)          echo "Unknown option: $1"; usage ;;
    esac
done

if $WANT_ALL; then
    WANT_GTK=yes
    WANT_QT=yes
fi

BINDIR="$PREFIX/bin"

# ── Helpers ──────────────────────────────────────────────────────────────────

info()  { echo -e "${CYAN}::${RESET} $*"; }
ok()    { echo -e "${GREEN}✓${RESET} $*"; }
warn()  { echo -e "${YELLOW}!${RESET} $*"; }
err()   { echo -e "${RED}✗${RESET} $*" >&2; }

confirm() {
    if $AUTO_YES; then return 0; fi
    local prompt="$1 [Y/n] "
    read -rp "$(echo -e "${BOLD}${prompt}${RESET}")" answer
    [[ -z "$answer" || "$answer" =~ ^[Yy] ]]
}

pick_one() {
    local prompt="$1"; shift
    local options=("$@")
    if $AUTO_YES; then echo "1"; return; fi
    echo -e "\n${BOLD}${prompt}${RESET}"
    for i in "${!options[@]}"; do
        echo "  $((i+1))) ${options[$i]}"
    done
    while true; do
        read -rp "$(echo -e "${BOLD}> ${RESET}")" choice
        if [[ "$choice" =~ ^[0-9]+$ ]] && (( choice >= 1 && choice <= ${#options[@]} )); then
            echo "$choice"
            return
        fi
        echo "  Please enter a number between 1 and ${#options[@]}"
    done
}

has_cmd() { command -v "$1" &>/dev/null; }

# ── Detect distro ────────────────────────────────────────────────────────────

detect_distro() {
    if [[ -f /etc/os-release ]]; then
        # shellcheck source=/dev/null
        . /etc/os-release
        case "${ID:-}" in
            arch|manjaro|endeavouros|garuda|artix|cachyos)  echo "arch" ;;
            fedora|nobara)                                   echo "fedora" ;;
            ubuntu|pop|linuxmint|elementary|zorin|neon)      echo "ubuntu" ;;
            debian|raspbian|kali|pureos)                     echo "debian" ;;
            opensuse*|sles)                                  echo "opensuse" ;;
            void)                                            echo "void" ;;
            alpine)                                          echo "alpine" ;;
            nixos)                                           echo "nix" ;;
            gentoo)                                          echo "gentoo" ;;
            *)
                # Check ID_LIKE as fallback
                case "${ID_LIKE:-}" in
                    *arch*)    echo "arch" ;;
                    *debian*)  echo "debian" ;;
                    *fedora*|*rhel*) echo "fedora" ;;
                    *suse*)    echo "opensuse" ;;
                    *)         echo "unknown" ;;
                esac
                ;;
        esac
    elif has_cmd pacman; then   echo "arch"
    elif has_cmd apt; then      echo "debian"
    elif has_cmd dnf; then      echo "fedora"
    elif has_cmd zypper; then   echo "opensuse"
    elif has_cmd xbps-install; then echo "void"
    elif has_cmd apk; then      echo "alpine"
    elif has_cmd nix-env; then  echo "nix"
    elif has_cmd emerge; then   echo "gentoo"
    else echo "unknown"
    fi
}

# ── Detect available toolkits ────────────────────────────────────────────────

has_gtk() {
    pkg-config --exists gtk4 2>/dev/null && pkg-config --exists libadwaita-1 2>/dev/null
}

has_qt() {
    pkg-config --exists Qt6Core 2>/dev/null && pkg-config --exists Qt6Quick 2>/dev/null
}

gtk_version() {
    pkg-config --modversion gtk4 2>/dev/null || echo "not installed"
}

qt_version() {
    pkg-config --modversion Qt6Core 2>/dev/null || echo "not installed"
}

adw_version() {
    pkg-config --modversion libadwaita-1 2>/dev/null || echo "not installed"
}

# ── Package names per distro ─────────────────────────────────────────────────

gtk_packages() {
    case "$1" in
        arch)     echo "gtk4 libadwaita" ;;
        fedora)   echo "gtk4-devel libadwaita-devel" ;;
        ubuntu|debian) echo "libgtk-4-dev libadwaita-1-dev" ;;
        opensuse) echo "gtk4-devel libadwaita-devel" ;;
        void)     echo "gtk4-devel libadwaita-devel" ;;
        alpine)   echo "gtk4.0-dev libadwaita-dev" ;;
        gentoo)   echo "gui-libs/gtk:4 gui-libs/libadwaita" ;;
        *)        echo "" ;;
    esac
}

qt_packages() {
    case "$1" in
        arch)     echo "qt6-base qt6-declarative qt6-quickcontrols2 cmake" ;;
        fedora)   echo "qt6-qtbase-devel qt6-qtdeclarative-devel cmake" ;;
        ubuntu|debian) echo "qt6-base-dev qt6-declarative-dev qml6-module-qtquick-controls cmake" ;;
        opensuse) echo "qt6-base-devel qt6-declarative-devel qt6-quickcontrols2-devel cmake" ;;
        void)     echo "qt6-base-devel qt6-declarative-devel cmake" ;;
        alpine)   echo "qt6-qtbase-dev qt6-qtdeclarative-dev cmake" ;;
        gentoo)   echo "dev-qt/qtbase:6 dev-qt/qtdeclarative:6 dev-build/cmake" ;;
        *)        echo "" ;;
    esac
}

ffmpeg_package() {
    case "$1" in
        arch)     echo "ffmpeg" ;;
        fedora)   echo "ffmpeg-free" ;;
        ubuntu|debian) echo "ffmpeg" ;;
        opensuse) echo "ffmpeg" ;;
        void)     echo "ffmpeg" ;;
        alpine)   echo "ffmpeg" ;;
        gentoo)   echo "media-video/ffmpeg" ;;
        *)        echo "" ;;
    esac
}

common_packages() {
    # Build essentials needed on some distros
    case "$1" in
        ubuntu|debian) echo "build-essential pkg-config" ;;
        fedora)        echo "gcc pkg-config" ;;
        opensuse)      echo "gcc pkg-config" ;;
        alpine)        echo "build-base pkgconf" ;;
        *)             echo "" ;;
    esac
}

install_cmd() {
    case "$1" in
        arch)     echo "sudo pacman -S --needed --noconfirm" ;;
        fedora)   echo "sudo dnf install -y" ;;
        ubuntu|debian) echo "sudo apt install -y" ;;
        opensuse) echo "sudo zypper install -y" ;;
        void)     echo "sudo xbps-install -y" ;;
        alpine)   echo "sudo apk add" ;;
        gentoo)   echo "sudo emerge --noreplace" ;;
        nix)      echo "nix-env -iA nixpkgs." ;;
        *)        echo "" ;;
    esac
}

# ── Install packages ─────────────────────────────────────────────────────────

install_packages() {
    local distro="$1"; shift
    local packages=("$@")

    if [[ ${#packages[@]} -eq 0 ]]; then return 0; fi

    local cmd
    cmd=$(install_cmd "$distro")
    if [[ -z "$cmd" ]]; then
        warn "Cannot auto-install packages for your distro."
        warn "Please install manually: ${packages[*]}"
        if ! confirm "Continue anyway?"; then exit 1; fi
        return 0
    fi

    info "Installing: ${packages[*]}"
    # shellcheck disable=SC2086
    $cmd ${packages[*]}
}

# ── Main ─────────────────────────────────────────────────────────────────────

main() {
    echo -e "\n${BOLD}╔══════════════════════════════════════╗${RESET}"
    echo -e "${BOLD}║       Traceless Install Script       ║${RESET}"
    echo -e "${BOLD}╚══════════════════════════════════════╝${RESET}\n"

    # --- Detect distro ---
    local distro
    distro=$(detect_distro)
    if [[ "$distro" == "unknown" ]]; then
        warn "Could not detect your Linux distribution."
        warn "You may need to install dependencies manually (see README)."
    else
        ok "Detected distro: ${BOLD}${distro}${RESET}"
    fi

    # --- Detect toolkits ---
    local gtk_available=false qt_available=false
    if has_gtk; then
        gtk_available=true
        ok "GTK4 found: $(gtk_version), libadwaita: $(adw_version)"
    else
        warn "GTK4/libadwaita development libraries not found"
    fi

    if has_qt; then
        qt_available=true
        ok "Qt6 found: $(qt_version)"
    else
        warn "Qt6 development libraries not found"
    fi

    # --- Decide what to build ---
    local build_gtk=false build_qt=false

    if [[ "$WANT_GTK" == "yes" ]]; then build_gtk=true; fi
    if [[ "$WANT_QT" == "yes" ]]; then build_qt=true; fi

    if [[ "$WANT_GTK" == "auto" && "$WANT_QT" == "auto" ]]; then
        if $gtk_available && $qt_available; then
            # Both available: build both automatically
            build_gtk=true
            build_qt=true
            info "Both toolkits detected, building both frontends"
        elif $gtk_available; then
            # Only GTK: build GTK automatically
            build_gtk=true
            info "GTK detected, building GTK frontend"
        elif $qt_available; then
            # Only Qt: build Qt automatically
            build_qt=true
            info "Qt detected, building Qt frontend"
        else
            # Nothing found: ask the user what to install
            warn "No toolkit detected."
            local choice
            choice=$(pick_one "Which frontend(s) should be installed?" \
                "Both GTK and Qt (recommended)" \
                "GTK only (GNOME / XFCE / Cinnamon / etc.)" \
                "Qt only (KDE Plasma / LXQt)")
            case "$choice" in
                1) build_gtk=true; build_qt=true ;;
                2) build_gtk=true ;;
                3) build_qt=true ;;
            esac
        fi
    fi

    if ! $build_gtk && ! $build_qt; then
        err "Nothing to build. Use --gtk, --qt, or --all."
        exit 1
    fi

    info "Will build: $( $build_gtk && echo -n "GTK " )$( $build_qt && echo -n "Qt " )frontend(s)"

    # --- Install system dependencies ---
    local packages_to_install=()

    # Common build tools
    local common
    common=$(common_packages "$distro")
    if [[ -n "$common" ]]; then
        for pkg in $common; do packages_to_install+=("$pkg"); done
    fi

    # GTK deps
    if $build_gtk && ! has_gtk; then
        local gtk_pkgs
        gtk_pkgs=$(gtk_packages "$distro")
        if [[ -n "$gtk_pkgs" ]]; then
            for pkg in $gtk_pkgs; do packages_to_install+=("$pkg"); done
        fi
    fi

    # Qt deps
    if $build_qt && ! has_qt; then
        local qt_pkgs
        qt_pkgs=$(qt_packages "$distro")
        if [[ -n "$qt_pkgs" ]]; then
            for pkg in $qt_pkgs; do packages_to_install+=("$pkg"); done
        fi
    fi

    # ffmpeg (optional, for video metadata)
    if ! has_cmd ffmpeg; then
        local ffmpeg_pkg
        ffmpeg_pkg=$(ffmpeg_package "$distro")
        if [[ -n "$ffmpeg_pkg" ]]; then
            packages_to_install+=("$ffmpeg_pkg")
        fi
    fi

    if [[ ${#packages_to_install[@]} -gt 0 ]]; then
        info "Installing system packages: ${packages_to_install[*]}"
        install_packages "$distro" "${packages_to_install[@]}"
        ok "System packages installed"
    else
        ok "All system dependencies are already installed"
    fi

    # --- Ensure Rust is available ---
    if ! has_cmd cargo; then
        info "Rust/Cargo not found, installing via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
        ok "Rust installed: $(rustc --version)"
    else
        ok "Rust found: $(rustc --version)"
    fi

    # Check minimum Rust version
    local rust_version
    rust_version=$(rustc --version | grep -oP '\d+\.\d+' | head -1)
    local rust_major rust_minor
    rust_major=$(echo "$rust_version" | cut -d. -f1)
    rust_minor=$(echo "$rust_version" | cut -d. -f2)
    if (( rust_major < 1 || (rust_major == 1 && rust_minor < 92) )); then
        info "Rust 1.92+ is required (you have ${rust_version}), updating..."
        rustup update stable
        ok "Rust updated: $(rustc --version)"
    fi

    # --- Build ---
    echo ""
    info "Building Traceless..."

    local build_packages=("-p" "traceless")  # always build the launcher

    if $build_gtk; then
        build_packages+=("-p" "traceless-gtk")
    fi
    if $build_qt; then
        build_packages+=("-p" "traceless-qt")
    fi

    # cd to script directory (where Cargo.toml lives)
    cd "$(dirname "$(realpath "$0")")"

    cargo build --release "${build_packages[@]}"
    ok "Build complete"

    # --- Install ---
    echo ""
    info "Installing to ${BINDIR}/"
    mkdir -p "$BINDIR"

    local installed=()

    if [[ "$PREFIX" == "/usr/local" || "$PREFIX" == "/usr" ]]; then
        sudo install -Dm755 target/release/traceless "$BINDIR/traceless"
        installed+=("traceless")

        if $build_gtk; then
            sudo install -Dm755 target/release/traceless-gtk "$BINDIR/traceless-gtk"
            installed+=("traceless-gtk")
        fi
        if $build_qt; then
            sudo install -Dm755 target/release/traceless-qt "$BINDIR/traceless-qt"
            installed+=("traceless-qt")
        fi
    else
        install -Dm755 target/release/traceless "$BINDIR/traceless"
        installed+=("traceless")

        if $build_gtk; then
            install -Dm755 target/release/traceless-gtk "$BINDIR/traceless-gtk"
            installed+=("traceless-gtk")
        fi
        if $build_qt; then
            install -Dm755 target/release/traceless-qt "$BINDIR/traceless-qt"
            installed+=("traceless-qt")
        fi
    fi

    # --- Summary ---
    echo ""
    echo -e "${GREEN}╔══════════════════════════════════════╗${RESET}"
    echo -e "${GREEN}║        Installation complete!        ║${RESET}"
    echo -e "${GREEN}╚══════════════════════════════════════╝${RESET}"
    echo ""
    echo -e "  Installed binaries:"
    for bin in "${installed[@]}"; do
        echo -e "    ${GREEN}✓${RESET} ${BINDIR}/${bin}"
    done
    echo ""

    # Check if BINDIR is in PATH
    if [[ ":$PATH:" != *":${BINDIR}:"* ]]; then
        warn "${BINDIR} is not in your PATH."
        echo "  Add it with:"
        echo "    export PATH=\"${BINDIR}:\$PATH\""
        echo "  Or add that line to your ~/.bashrc or ~/.zshrc"
        echo ""
    fi

    echo -e "  Run with:"
    echo -e "    ${BOLD}traceless${RESET}           # auto-detect desktop environment"
    if $build_gtk; then
        echo -e "    ${BOLD}traceless-gtk${RESET}       # force GTK/GNOME frontend"
    fi
    if $build_qt; then
        echo -e "    ${BOLD}traceless-qt${RESET}        # force Qt/KDE frontend"
    fi
    echo ""
}

main
