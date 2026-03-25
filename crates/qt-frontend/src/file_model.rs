use std::path::PathBuf;
use std::sync::mpsc;

use traceless_core::{FileStore, FileStoreEvent};

use crate::bridge::EventBridge;

#[cxx_qt::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, file_count)]
        #[qproperty(bool, is_working)]
        #[qproperty(QString, status_message)]
        type FileListModel = super::FileListModelRust;
    }

    unsafe extern "RustQt" {
        #[qinvokable]
        fn add_files(self: Pin<&mut FileListModel>, paths: &QString);

        #[qinvokable]
        fn add_folder(self: Pin<&mut FileListModel>, path: &QString);

        #[qinvokable]
        fn remove_file(self: Pin<&mut FileListModel>, index: i32);

        #[qinvokable]
        fn clear_files(self: Pin<&mut FileListModel>);

        #[qinvokable]
        fn clean_all(self: Pin<&mut FileListModel>);

        #[qinvokable]
        fn get_filename(self: &FileListModel, index: i32) -> QString;

        #[qinvokable]
        fn get_directory(self: &FileListModel, index: i32) -> QString;

        #[qinvokable]
        fn get_simple_state(self: &FileListModel, index: i32) -> QString;

        #[qinvokable]
        fn get_metadata_count(self: &FileListModel, index: i32) -> i32;

        #[qinvokable]
        fn poll_events(self: Pin<&mut FileListModel>);

        #[qinvokable]
        fn set_lightweight_mode(self: Pin<&mut FileListModel>, enabled: bool);
    }
}

use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;
use std::pin::Pin;

#[derive(Default)]
pub struct FileListModelRust {
    store: FileStore,
    tx: Option<mpsc::Sender<FileStoreEvent>>,
    bridge: Option<EventBridge>,
    file_count: i32,
    is_working: bool,
    status_message: QString,
}

impl FileListModelRust {
    fn init_channel(&mut self) {
        if self.tx.is_none() {
            let (tx, rx) = mpsc::channel();
            self.tx = Some(tx);
            self.bridge = Some(EventBridge::new(rx));
        }
    }

    fn update_properties(&mut self) {
        self.file_count = self.store.len() as i32;
        self.is_working = self.store.has_working();
        let cleaned = self.store.cleaned_count();
        if cleaned > 0 && !self.store.has_working() {
            self.status_message = QString::from(&format!(
                "{cleaned} file{} cleaned.",
                if cleaned == 1 { "" } else { "s" }
            ) as &str);
        } else {
            self.status_message = QString::from("");
        }
    }
}

impl ffi::FileListModel {
    fn add_files(mut self: Pin<&mut Self>, paths: &QString) {
        self.as_mut().rust_mut().init_channel();
        let path_str = paths.to_string();
        let path_list: Vec<PathBuf> = path_str
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();

        if let Some(tx) = self.as_mut().rust_mut().tx.clone() {
            self.as_mut().rust_mut().store.add_files(path_list, tx);
        }
        self.as_mut().rust_mut().update_properties();
    }

    fn add_folder(mut self: Pin<&mut Self>, path: &QString) {
        self.as_mut().rust_mut().init_channel();
        let path = PathBuf::from(path.to_string());
        if let Some(tx) = self.as_mut().rust_mut().tx.clone() {
            self.as_mut().rust_mut().store.add_directory(&path, true, tx);
        }
        self.as_mut().rust_mut().update_properties();
    }

    fn remove_file(mut self: Pin<&mut Self>, index: i32) {
        self.as_mut().rust_mut().store.remove_file(index as usize);
        self.as_mut().rust_mut().update_properties();
    }

    fn clear_files(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().store.clear();
        self.as_mut().rust_mut().update_properties();
    }

    fn clean_all(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().init_channel();
        if let Some(tx) = self.as_mut().rust_mut().tx.clone() {
            self.as_mut().rust_mut().store.clean_files(tx);
        }
        self.as_mut().rust_mut().update_properties();
    }

    fn get_filename(&self, index: i32) -> QString {
        self.rust()
            .store
            .get(index as usize)
            .map(|e| QString::from(&e.filename as &str))
            .unwrap_or_default()
    }

    fn get_directory(&self, index: i32) -> QString {
        self.rust()
            .store
            .get(index as usize)
            .map(|e| QString::from(&e.directory as &str))
            .unwrap_or_default()
    }

    fn get_simple_state(&self, index: i32) -> QString {
        self.rust()
            .store
            .get(index as usize)
            .map(|e| QString::from(e.state.simple_state()))
            .unwrap_or_default()
    }

    fn get_metadata_count(&self, index: i32) -> i32 {
        self.rust()
            .store
            .get(index as usize)
            .map(|e| e.total_metadata() as i32)
            .unwrap_or(0)
    }

    fn poll_events(mut self: Pin<&mut Self>) {
        let events: Vec<FileStoreEvent> = if let Some(ref bridge) = self.rust().bridge {
            bridge.drain_events()
        } else {
            return;
        };

        for event in events {
            self.as_mut().rust_mut().store.apply_event(&event);
        }
        self.as_mut().rust_mut().update_properties();
    }

    fn set_lightweight_mode(mut self: Pin<&mut Self>, enabled: bool) {
        self.as_mut().rust_mut().store.lightweight_mode = enabled;
    }
}
