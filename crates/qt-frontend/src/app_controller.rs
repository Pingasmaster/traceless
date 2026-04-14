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
        type AppController = super::AppControllerRust;
    }

    unsafe extern "RustQt" {
        #[qinvokable]
        fn get_supported_extensions(self: &AppController) -> QString;
    }
}

use cxx_qt_lib::QString;

pub struct AppControllerRust {
    /// Populated at construction from `env!("CARGO_PKG_VERSION")` so
    /// QML bindings like `text: "Version " + appController.app_version`
    /// track the workspace manifest automatically. Previously derived
    /// as `Default`, which left the property as an empty string and
    /// forced QML to hardcode the version in the About dialog.
    app_version: QString,
}

impl Default for AppControllerRust {
    fn default() -> Self {
        Self {
            app_version: QString::from(env!("CARGO_PKG_VERSION")),
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
}
