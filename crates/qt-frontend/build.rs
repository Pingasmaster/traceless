use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(QmlModule::new("Traceless").qml_files([
        "qml/main.qml",
        "qml/EmptyView.qml",
        "qml/FilesView.qml",
        "qml/FileDelegate.qml",
        "qml/DetailsPanel.qml",
        "qml/MetadataSection.qml",
        "qml/Badge.qml",
        "qml/StatusBar.qml",
        "qml/CleaningWarningDialog.qml",
    ]))
    .file("src/file_model.rs")
    .file("src/app_controller.rs")
    .build();
}
