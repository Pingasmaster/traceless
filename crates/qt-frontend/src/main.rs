mod app_controller;
mod bridge;
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
