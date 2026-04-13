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

#[derive(Default)]
pub struct AppControllerRust {
    app_version: QString,
}

impl ffi::AppController {
    // `&self` is required by `#[qinvokable]` even though the body is stateless.
    #[allow(clippy::unused_self)]
    fn get_supported_extensions(&self) -> QString {
        let exts = traceless_core::format_support::supported_extensions();
        QString::from(&exts.join(", ") as &str)
    }
}
