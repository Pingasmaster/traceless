// `deny` rather than `forbid` so the #[cfg(test)] module below can
// flip unsafe on locally for `env::set_var` / `env::remove_var`,
// which became unsafe in edition 2024. Non-test code stays unsafe-free.
#![deny(unsafe_code)]
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
                "GNOME" | "UNITY" | "CINNAMON" | "MATE" | "XFCE" | "BUDGIE" | "PANTHEON"
                | "COSMIC" | "DEEPIN" | "ENLIGHTENMENT" => {
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
            eprintln!("warning: {binary_name} not found, falling back to {fallback_name}");
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
    let status = Command::new(exe).args(&args).status().unwrap_or_else(|e| {
        eprintln!("Failed to launch {}: {e}", exe.display());
        std::process::exit(1);
    });
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Every test in this module mutates process-wide env vars, which
    /// cargo runs in parallel by default. A single mutex serializes
    /// the whole group so an `XDG_CURRENT_DESKTOP` set in one test
    /// cannot be observed by another before the cleanup runs.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Clear every env var the detector consults so each test starts
    /// from a known-empty baseline before setting only the subset it
    /// cares about.
    fn clear_detector_env() {
        // SAFETY: tests holding `env_lock()` are the sole accessors.
        unsafe {
            env::remove_var("TRACELESS_FRONTEND");
            env::remove_var("XDG_CURRENT_DESKTOP");
            env::remove_var("KDE_SESSION_VERSION");
            env::remove_var("KDE_FULL_SESSION");
            env::remove_var("GNOME_DESKTOP_SESSION_ID");
        }
    }

    fn set(var: &str, val: &str) {
        // SAFETY: tests holding `env_lock()` are the sole accessors.
        unsafe {
            env::set_var(var, val);
        }
    }

    fn is_gtk(de: &DesktopEnvironment) -> bool {
        matches!(de, DesktopEnvironment::Gtk)
    }
    fn is_qt(de: &DesktopEnvironment) -> bool {
        matches!(de, DesktopEnvironment::Qt)
    }

    // ---------- Explicit TRACELESS_FRONTEND override ----------

    #[test]
    fn override_gtk_is_respected() {
        let _lock = env_lock();
        clear_detector_env();
        set("TRACELESS_FRONTEND", "gtk");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn override_gnome_is_respected() {
        let _lock = env_lock();
        clear_detector_env();
        set("TRACELESS_FRONTEND", "gnome");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn override_qt_is_respected() {
        let _lock = env_lock();
        clear_detector_env();
        set("TRACELESS_FRONTEND", "qt");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn override_kde_is_respected() {
        let _lock = env_lock();
        clear_detector_env();
        set("TRACELESS_FRONTEND", "kde");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn override_is_case_insensitive() {
        let _lock = env_lock();
        clear_detector_env();
        set("TRACELESS_FRONTEND", "GTK");
        assert!(is_gtk(&detect_desktop_environment()));
        set("TRACELESS_FRONTEND", "QT");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn override_with_garbage_falls_through_to_detection() {
        // Invalid value should not force a choice; the detector
        // continues down the chain and eventually returns GTK as
        // the final fallback when nothing else matches.
        let _lock = env_lock();
        clear_detector_env();
        set("TRACELESS_FRONTEND", "nonsense-value");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    // ---------- XDG_CURRENT_DESKTOP matches per family ----------

    #[test]
    fn xdg_gnome_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "GNOME");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_kde_routes_to_qt() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "KDE");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_plasma_routes_to_qt() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "PLASMA");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_lxqt_routes_to_qt() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "LXQT");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_xfce_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "XFCE");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_cinnamon_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "CINNAMON");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_mate_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "MATE");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_budgie_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "BUDGIE");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_pantheon_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "PANTHEON");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_unity_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "UNITY");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_cosmic_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "COSMIC");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_lowercase_is_normalised() {
        // The detector uppercases the whole string before splitting,
        // so `gnome` and `kde` should route the same as `GNOME` / `KDE`.
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "gnome");
        assert!(is_gtk(&detect_desktop_environment()));
        set("XDG_CURRENT_DESKTOP", "kde");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_colon_separated_picks_first_recognised_family() {
        // Real-world string on Ubuntu: "ubuntu:GNOME". The first
        // component is unknown so the detector must keep walking and
        // pick GNOME.
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "ubuntu:GNOME");
        assert!(is_gtk(&detect_desktop_environment()));
        set("XDG_CURRENT_DESKTOP", "pop:COSMIC");
        assert!(is_gtk(&detect_desktop_environment()));
        set("XDG_CURRENT_DESKTOP", "neon:KDE:PLASMA");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn xdg_unknown_falls_through_to_kde_env_check() {
        let _lock = env_lock();
        clear_detector_env();
        set("XDG_CURRENT_DESKTOP", "tiling-wm-of-the-year");
        set("KDE_SESSION_VERSION", "5");
        assert!(is_qt(&detect_desktop_environment()));
    }

    // ---------- KDE_* fallbacks ----------

    #[test]
    fn kde_session_version_routes_to_qt() {
        let _lock = env_lock();
        clear_detector_env();
        set("KDE_SESSION_VERSION", "5");
        assert!(is_qt(&detect_desktop_environment()));
    }

    #[test]
    fn kde_full_session_routes_to_qt() {
        let _lock = env_lock();
        clear_detector_env();
        set("KDE_FULL_SESSION", "true");
        assert!(is_qt(&detect_desktop_environment()));
    }

    // ---------- GNOME_DESKTOP_SESSION_ID fallback ----------

    #[test]
    fn gnome_desktop_session_id_routes_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        set("GNOME_DESKTOP_SESSION_ID", "this-is-deprecated");
        assert!(is_gtk(&detect_desktop_environment()));
    }

    // ---------- Final fallback ----------

    #[test]
    fn empty_environment_defaults_to_gtk() {
        let _lock = env_lock();
        clear_detector_env();
        assert!(is_gtk(&detect_desktop_environment()));
    }
}
