// cxx-qt's `#[cxx_qt::bridge]` macro generates `Box<...Rust>` return types as
// part of its FFI model. That pattern trips `unnecessary_box_returns` on every
// bridge module, and the macro rejects per-module `#[allow]` attributes, so we
// suppress it at the crate root.
#![allow(clippy::unnecessary_box_returns)]
// See CLAUDE.md: transitive dep version duplication we cannot fix.
#![allow(clippy::multiple_crate_versions)]

mod app_controller;
mod file_model;
mod metadata_model;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};

fn main() {
    env_logger::init();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/Traceless/qml/main.qml"));
    }

    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
