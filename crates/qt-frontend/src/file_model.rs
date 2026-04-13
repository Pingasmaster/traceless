use std::path::PathBuf;
use std::pin::Pin;
use std::thread;

use async_channel::Sender;
use traceless_core::{FileStore, FileStoreEvent};

#[cxx_qt::bridge]
mod ffi {
    unsafe extern "C++Qt" {
        include!(<QtCore/QAbstractListModel>);
        #[qobject]
        type QAbstractListModel;
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;

        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;

        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;

        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;

        include!("cxx-qt-lib/qvector.h");
        type QVector_i32 = cxx_qt_lib::QVector<i32>;
    }

    #[qenum(FileListModel)]
    enum Role {
        Filename,
        Directory,
        SimpleState,
        MetadataCount,
    }

    extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qproperty(i32, file_count)]
        #[qproperty(bool, is_working)]
        #[qproperty(QString, status_message)]
        type FileListModel = super::FileListModelRust;
    }

    impl cxx_qt::Threading for FileListModel {}

    unsafe extern "RustQt" {
        #[inherit]
        #[qsignal]
        #[cxx_name = "dataChanged"]
        fn data_changed(
            self: Pin<&mut FileListModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QVector_i32,
        );
    }

    extern "RustQt" {
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
        fn set_lightweight_mode(self: Pin<&mut FileListModel>, enabled: bool);
    }

    extern "RustQt" {
        #[qinvokable]
        #[cxx_override]
        fn data(self: &FileListModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &FileListModel) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &FileListModel, parent: &QModelIndex) -> i32;
    }

    extern "RustQt" {
        /// # Safety
        /// Inherited beginInsertRows from QAbstractItemModel. Caller must pair with end_insert_rows.
        #[inherit]
        #[cxx_name = "beginInsertRows"]
        unsafe fn begin_insert_rows(
            self: Pin<&mut FileListModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );

        /// # Safety
        /// Inherited endInsertRows. Must be called after a matching begin_insert_rows.
        #[inherit]
        #[cxx_name = "endInsertRows"]
        unsafe fn end_insert_rows(self: Pin<&mut FileListModel>);

        /// # Safety
        /// Inherited beginRemoveRows. Caller must pair with end_remove_rows.
        #[inherit]
        #[cxx_name = "beginRemoveRows"]
        unsafe fn begin_remove_rows(
            self: Pin<&mut FileListModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );

        /// # Safety
        /// Inherited endRemoveRows. Must be called after a matching begin_remove_rows.
        #[inherit]
        #[cxx_name = "endRemoveRows"]
        unsafe fn end_remove_rows(self: Pin<&mut FileListModel>);

        /// # Safety
        /// Inherited beginResetModel. Caller must pair with end_reset_model.
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut FileListModel>);

        /// # Safety
        /// Inherited endResetModel. Must be called after a matching begin_reset_model.
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut FileListModel>);
    }

    unsafe extern "RustQt" {
        #[inherit]
        fn index(
            self: &FileListModel,
            row: i32,
            column: i32,
            parent: &QModelIndex,
        ) -> QModelIndex;
    }
}

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant, QVector};

#[derive(Default)]
pub struct FileListModelRust {
    store: FileStore,
    tx: Option<Sender<FileStoreEvent>>,
    file_count: i32,
    is_working: bool,
    status_message: QString,
}

impl ffi::FileListModel {
    fn ensure_channel(mut self: Pin<&mut Self>) {
        if self.rust().tx.is_some() {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<FileStoreEvent>();
        let qt_thread = self.qt_thread();

        // Dedicated bridge thread: drains events from the core workers and
        // queues each one onto the Qt event loop via Qt::QueuedConnection
        // semantics. Exits cleanly when every tx drops (model destroyed).
        thread::spawn(move || {
            while let Ok(event) = rx.recv_blocking() {
                let _ = qt_thread.queue(move |mut this: Pin<&mut Self>| {
                    let affected_index: Option<i32> = match &event {
                        FileStoreEvent::FileStateChanged { index, .. }
                        | FileStoreEvent::MetadataReady { index, .. }
                        | FileStoreEvent::FileError { index, .. } => {
                            i32::try_from(*index).ok()
                        }
                        FileStoreEvent::AllDone => None,
                    };
                    this.as_mut().rust_mut().store.apply_event(&event);
                    if let Some(row) = affected_index {
                        let model_index = this.index(row, 0, &QModelIndex::default());
                        let mut roles = QVector::<i32>::default();
                        roles.append(ffi::Role::Filename.repr);
                        roles.append(ffi::Role::Directory.repr);
                        roles.append(ffi::Role::SimpleState.repr);
                        roles.append(ffi::Role::MetadataCount.repr);
                        this.as_mut().data_changed(&model_index, &model_index, &roles);
                    }
                    this.as_mut().update_aux_properties();
                });
            }
        });

        self.as_mut().rust_mut().tx = Some(tx);
    }

    fn update_aux_properties(mut self: Pin<&mut Self>) {
        let count = i32::try_from(self.rust().store.len()).unwrap_or(i32::MAX);
        let working = self.rust().store.has_working();
        let cleaned = self.rust().store.cleaned_count();
        self.as_mut().set_file_count(count);
        self.as_mut().set_is_working(working);
        let msg = if cleaned > 0 && !working {
            QString::from(&format!(
                "{cleaned} file{} cleaned.",
                if cleaned == 1 { "" } else { "s" }
            ) as &str)
        } else {
            QString::default()
        };
        self.as_mut().set_status_message(msg);
    }

    fn add_files(mut self: Pin<&mut Self>, paths: &QString) {
        self.as_mut().ensure_channel();
        let path_str = paths.to_string();
        let path_list: Vec<PathBuf> = path_str
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();

        if path_list.is_empty() {
            return;
        }

        let start = i32::try_from(self.rust().store.len()).unwrap_or(i32::MAX);
        let end = start + i32::try_from(path_list.len()).unwrap_or(0) - 1;

        unsafe {
            self.as_mut()
                .begin_insert_rows(&QModelIndex::default(), start, end);
        }
        if let Some(tx) = self.as_mut().rust_mut().tx.clone() {
            self.as_mut().rust_mut().store.add_files(path_list, &tx);
        }
        unsafe {
            self.as_mut().end_insert_rows();
        }

        self.as_mut().update_aux_properties();
    }

    fn add_folder(mut self: Pin<&mut Self>, path: &QString) {
        self.as_mut().ensure_channel();
        let path = PathBuf::from(path.to_string());

        let Some(tx) = self.as_mut().rust_mut().tx.clone() else {
            self.as_mut().update_aux_properties();
            return;
        };

        let start = i32::try_from(self.rust().store.len()).unwrap_or(i32::MAX);
        let pre_count = self.rust().store.len();
        self.as_mut().rust_mut().store.add_directory(&path, true, &tx);
        let added = self.rust().store.len().saturating_sub(pre_count);

        if added > 0 {
            let end = start + i32::try_from(added).unwrap_or(0) - 1;
            // add_directory already mutated the store; we announce the insertion
            // retroactively, which still triggers the ListView to pick up the
            // new rows because the model internals haven't been observed yet.
            unsafe {
                self.as_mut()
                    .begin_insert_rows(&QModelIndex::default(), start, end);
                self.as_mut().end_insert_rows();
            }
        }

        self.as_mut().update_aux_properties();
    }

    fn remove_file(mut self: Pin<&mut Self>, index: i32) {
        let Ok(idx) = usize::try_from(index) else {
            return;
        };
        if idx >= self.rust().store.len() {
            return;
        }
        unsafe {
            self.as_mut()
                .begin_remove_rows(&QModelIndex::default(), index, index);
            self.as_mut().rust_mut().store.remove_file(idx);
            self.as_mut().end_remove_rows();
        }
        self.as_mut().update_aux_properties();
    }

    fn clear_files(mut self: Pin<&mut Self>) {
        unsafe {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().store.clear();
            self.as_mut().end_reset_model();
        }
        self.as_mut().update_aux_properties();
    }

    fn clean_all(mut self: Pin<&mut Self>) {
        self.as_mut().ensure_channel();
        if let Some(tx) = self.as_mut().rust_mut().tx.clone() {
            self.as_mut().rust_mut().store.clean_files(&tx);
        }
        self.as_mut().update_aux_properties();
    }

    fn set_lightweight_mode(mut self: Pin<&mut Self>, enabled: bool) {
        self.as_mut().rust_mut().store.lightweight_mode = enabled;
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Ok(row) = usize::try_from(index.row()) else {
            return QVariant::default();
        };
        let Some(entry) = self.rust().store.get(row) else {
            return QVariant::default();
        };
        let role = ffi::Role { repr: role };
        match role {
            ffi::Role::Filename => QVariant::from(&QString::from(&entry.filename as &str)),
            ffi::Role::Directory => QVariant::from(&QString::from(&entry.directory as &str)),
            ffi::Role::SimpleState => {
                QVariant::from(&QString::from(entry.state.simple_state()))
            }
            ffi::Role::MetadataCount => {
                QVariant::from(&i32::try_from(entry.total_metadata()).unwrap_or(i32::MAX))
            }
            _ => QVariant::default(),
        }
    }

    // cxx-qt requires the override of QAbstractListModel::roleNames to be an
    // instance method, so &self is mandated by the bridge signature even though
    // the role mapping is entirely static.
    #[allow(clippy::unused_self)]
    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ffi::Role::Filename.repr, QByteArray::from("filename"));
        roles.insert(ffi::Role::Directory.repr, QByteArray::from("directory"));
        roles.insert(ffi::Role::SimpleState.repr, QByteArray::from("simpleState"));
        roles.insert(
            ffi::Role::MetadataCount.repr,
            QByteArray::from("metadataCount"),
        );
        roles
    }

    fn row_count(&self, parent: &QModelIndex) -> i32 {
        // Only the root has children in a list model.
        if parent.row() >= 0 {
            return 0;
        }
        i32::try_from(self.rust().store.len()).unwrap_or(i32::MAX)
    }
}
