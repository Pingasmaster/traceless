#![forbid(unsafe_code)]
// See CLAUDE.md: transitive dep version duplication we cannot fix.
#![allow(clippy::multiple_crate_versions)]

use std::env;
use std::ffi::OsString;
use std::process::Command;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

enum DesktopEnvironment {
    Gtk,
    Qt,
}

fn detect_desktop_environment() -> DesktopEnvironment {
    // Check for explicit override
    if let Ok(forced) = env::var("TRACELESS_FRONTEND") {
        match forced.to_lowercase().as_str() {
            "gtk" | "gnome" => return DesktopEnvironment::Gtk,
            "qt" | "kde" => return DesktopEnvironment::Qt,
            _ => {}
        }
    }

    // Check XDG_CURRENT_DESKTOP (colon-separated, e.g. "ubuntu:GNOME")
    if let Ok(xdg) = env::var("XDG_CURRENT_DESKTOP") {
        let upper = xdg.to_uppercase();
        for de in upper.split(':') {
            match de.trim() {
                "KDE" | "PLASMA" | "LXQT" => return DesktopEnvironment::Qt,
                "GNOME" | "UNITY" | "CINNAMON" | "MATE" | "XFCE" | "BUDGIE"
                | "PANTHEON" | "COSMIC" | "DEEPIN" | "ENLIGHTENMENT" => {
                    return DesktopEnvironment::Gtk;
                }
                _ => {}
            }
        }
    }

    // Fallback: check KDE-specific env vars
    if env::var("KDE_SESSION_VERSION").is_ok() || env::var("KDE_FULL_SESSION").is_ok() {
        return DesktopEnvironment::Qt;
    }

    // Fallback: check GNOME-specific env vars
    if env::var("GNOME_DESKTOP_SESSION_ID").is_ok() {
        return DesktopEnvironment::Gtk;
    }

    // Final fallback: GTK (most common on Linux)
    DesktopEnvironment::Gtk
}

fn main() {
    let frontend = detect_desktop_environment();

    let binary_name = match frontend {
        DesktopEnvironment::Gtk => "traceless-gtk",
        DesktopEnvironment::Qt => "traceless-qt",
    };

    // Resolve binary path: same directory as the launcher
    let current_exe = env::current_exe().expect("Failed to get current executable path");
    let exe_dir = current_exe
        .parent()
        .expect("Failed to get executable directory");
    let target_exe = exe_dir.join(binary_name);

    if target_exe.exists() {
        launch(&target_exe);
    } else {
        // Try the other frontend as fallback
        let fallback_name = match frontend {
            DesktopEnvironment::Gtk => "traceless-qt",
            DesktopEnvironment::Qt => "traceless-gtk",
        };
        let fallback_exe = exe_dir.join(fallback_name);

        if fallback_exe.exists() {
            // Use stderr directly rather than `log::warn!`: env_logger's
            // default filter is `Error`, so a `warn!` message never
            // reaches the user. The launcher is a CLI shim whose only
            // user-facing output channel is stderr, and a fallback
            // notice is genuinely useful information for someone who
            // expected a different frontend to launch.
            eprintln!(
                "warning: {binary_name} not found, falling back to {fallback_name}"
            );
            launch(&fallback_exe);
        } else {
            eprintln!(
                "Error: Neither {binary_name} nor {fallback_name} found in {}",
                exe_dir.display()
            );
            std::process::exit(1);
        }
    }
}

#[cfg(unix)]
fn launch(exe: &std::path::Path) -> ! {
    // `env::args_os()` preserves non-UTF-8 bytes; `env::args()` would
    // panic on any argv component that isn't valid UTF-8, which is legal
    // on POSIX filesystems.
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let err = Command::new(exe).args(&args).exec();
    eprintln!("Failed to launch {}: {err}", exe.display());
    std::process::exit(1);
}

#[cfg(not(unix))]
fn launch(exe: &std::path::Path) {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let status = Command::new(exe)
        .args(&args)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to launch {}: {e}", exe.display());
            std::process::exit(1);
        });
    std::process::exit(status.code().unwrap_or(1));
}
