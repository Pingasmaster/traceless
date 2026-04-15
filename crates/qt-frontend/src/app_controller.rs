#[cxx_qt::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, app_version)]
        #[qproperty(bool, limits_disabled)]
        type AppController = super::AppControllerRust;
    }

    unsafe extern "RustQt" {
        #[qinvokable]
        fn get_supported_extensions(self: &AppController) -> QString;

        /// Human-readable description of the per-file input cap, e.g.
        /// `"10 GiB"`. Read by the Preferences dialog so the UI text
        /// stays aligned with the core library's actual constant.
        #[qinvokable]
        fn limit_input_size(self: &AppController) -> QString;

        /// Human-readable description of the per-handler wall-clock cap.
        #[qinvokable]
        fn limit_handler_timeout(self: &AppController) -> QString;

        /// Human-readable description of the per-archive-member cap.
        #[qinvokable]
        fn limit_entry_decompressed(self: &AppController) -> QString;

        /// Human-readable description of the outer tar decompression cap.
        #[qinvokable]
        fn limit_tar_decompressed(self: &AppController) -> QString;

        /// Human-readable description of the cumulative archive cap.
        #[qinvokable]
        fn limit_archive_total_decompressed(self: &AppController) -> QString;

        /// Flip the process-wide "disable all limits" flag. Takes
        /// effect on the next handler call.
        #[qinvokable]
        fn set_limits_disabled_flag(self: Pin<&mut AppController>, disabled: bool);
    }
}

use std::pin::Pin;

use cxx_qt_lib::QString;

pub struct AppControllerRust {
    /// Populated at construction from `env!("CARGO_PKG_VERSION")` so
    /// QML bindings like `text: "Version " + appController.app_version`
    /// track the workspace manifest automatically. Previously derived
    /// as `Default`, which left the property as an empty string and
    /// forced QML to hardcode the version in the About dialog.
    app_version: QString,
    /// Cached mirror of the core library's process-wide
    /// `limits_disabled` atomic. Read at construction so the
    /// Preferences switch shows the real state at startup, and kept
    /// in sync via `set_limits_disabled_flag` from QML. The source of
    /// truth for the caps is still the atomic in `traceless_core::config`;
    /// this property exists only so QML has a QProperty to bind its
    /// Switch `checked` state to.
    limits_disabled: bool,
}

impl Default for AppControllerRust {
    fn default() -> Self {
        Self {
            app_version: QString::from(env!("CARGO_PKG_VERSION")),
            limits_disabled: traceless_core::limits_disabled(),
        }
    }
}

impl ffi::AppController {
    // `&self` is required by `#[qinvokable]` even though the body is stateless.
    #[allow(clippy::unused_self)]
    fn get_supported_extensions(&self) -> QString {
        let exts = traceless_core::format_support::supported_extensions();
        QString::from(&exts.join(", ") as &str)
    }

    #[allow(clippy::unused_self)]
    fn limit_input_size(&self) -> QString {
        QString::from(&format_bytes(traceless_core::MAX_INPUT_FILE_BYTES) as &str)
    }

    #[allow(clippy::unused_self)]
    fn limit_handler_timeout(&self) -> QString {
        QString::from(
            &format!("{} seconds", traceless_core::HANDLER_WALL_CLOCK_CAP.as_secs()) as &str,
        )
    }

    #[allow(clippy::unused_self)]
    fn limit_entry_decompressed(&self) -> QString {
        QString::from(&format_bytes(traceless_core::MAX_ENTRY_DECOMPRESSED_BYTES) as &str)
    }

    #[allow(clippy::unused_self)]
    fn limit_tar_decompressed(&self) -> QString {
        QString::from(&format_bytes(traceless_core::MAX_TAR_DECOMPRESSED_BYTES) as &str)
    }

    #[allow(clippy::unused_self)]
    fn limit_archive_total_decompressed(&self) -> QString {
        QString::from(&format_bytes(traceless_core::MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES) as &str)
    }

    fn set_limits_disabled_flag(self: Pin<&mut Self>, disabled: bool) {
        traceless_core::set_limits_disabled(disabled);
        self.set_limits_disabled(disabled);
    }
}

/// Render a byte count the same way the GNOME file managers do - IEC
/// binary prefixes (KiB/MiB/GiB). Kept module-local because only the
/// AppController limit helpers need it.
fn format_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        let whole = n / GIB;
        let rem = (n % GIB) * 10 / GIB;
        if rem == 0 {
            format!("{whole} GiB")
        } else {
            format!("{whole}.{rem} GiB")
        }
    } else if n >= MIB {
        let whole = n / MIB;
        let rem = (n % MIB) * 10 / MIB;
        if rem == 0 {
            format!("{whole} MiB")
        } else {
            format!("{whole}.{rem} MiB")
        }
    } else if n >= KIB {
        format!("{} KiB", n / KIB)
    } else {
        format!("{n} B")
    }
}
