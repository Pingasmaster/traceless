use std::path::PathBuf;
use std::pin::Pin;
use std::thread;

use async_channel::Sender;
use traceless_core::{collect_paths, FileStore, FileStoreEvent};

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
        #[qproperty(i32, detail_row)]
        #[qproperty(QString, detail_group)]
        #[qproperty(i32, detail_count)]
        #[qproperty(QString, detail_error)]
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

        /// Populate the detail_* properties from the given row of the
        /// underlying `FileStore`. Passing `-1` clears the details.
        #[qinvokable]
        fn select_detail(self: Pin<&mut FileListModel>, row: i32);

        /// Read a metadata key for the currently selected detail row.
        #[qinvokable]
        fn detail_key(self: &FileListModel, index: i32) -> QString;

        /// Read a metadata value for the currently selected detail row.
        #[qinvokable]
        fn detail_value(self: &FileListModel, index: i32) -> QString;
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

pub struct FileListModelRust {
    store: FileStore,
    tx: Option<Sender<FileStoreEvent>>,
    file_count: i32,
    is_working: bool,
    status_message: QString,
    /// Currently selected detail row, or -1 when the details drawer is empty.
    detail_row: i32,
    detail_group: QString,
    detail_count: i32,
    /// Flat buffer of (key, value) pairs captured the moment the user
    /// selected a detail row. Captured into the model so that QML can
    /// render from it even if the underlying file state mutates later.
    detail_items: Vec<(String, String)>,
    /// User-facing error attached to the currently selected detail row.
    /// Held as `QString` (and exposed via `#[qproperty]`) so its QML
    /// bindings re-evaluate when `select_detail` writes a new value.
    /// The old Q_INVOKABLE version was bound as
    /// `text: model.detail_error()` in `DetailsPanel.qml`, which is
    /// evaluated exactly once at Label creation (QML doesn't track
    /// method-call return values as binding dependencies), so the
    /// drawer would freeze the error text from the first file the
    /// user ever viewed.
    detail_error: QString,
}

impl Default for FileListModelRust {
    fn default() -> Self {
        Self {
            store: FileStore::default(),
            tx: None,
            file_count: 0,
            is_working: false,
            status_message: QString::default(),
            detail_row: -1,
            detail_group: QString::default(),
            detail_count: 0,
            detail_items: Vec::new(),
            detail_error: QString::default(),
        }
    }
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
                    // `apply_event` resolves the stable FileId to a row
                    // index, returning None for stale events whose file
                    // was removed before the event was delivered.
                    let affected_row = this
                        .as_mut()
                        .rust_mut()
                        .store
                        .apply_event(&event)
                        .and_then(|idx| i32::try_from(idx).ok());
                    if let Some(row) = affected_row {
                        let model_index = this.index(row, 0, &QModelIndex::default());
                        let mut roles = QVector::<i32>::default();
                        roles.append(ffi::Role::Filename.repr);
                        roles.append(ffi::Role::Directory.repr);
                        roles.append(ffi::Role::SimpleState.repr);
                        roles.append(ffi::Role::MetadataCount.repr);
                        this.as_mut().data_changed(&model_index, &model_index, &roles);
                    }
                    this.as_mut().update_aux_properties();

                    // If the event updated the row whose metadata is
                    // currently being rendered in the detail drawer,
                    // re-snapshot so the drawer reflects the new state
                    // instead of whatever was captured when the user
                    // clicked Details. Without this, a worker that
                    // finishes scanning the selected row leaves the
                    // drawer stuck on its pre-event snapshot.
                    if let Some(row) = affected_row {
                        let current_detail = this.rust().detail_row;
                        if row == current_detail {
                            this.as_mut().select_detail(row);
                        }
                    }
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
        // The QML bridge joins paths with a NUL byte. NUL is not a legal
        // filename byte on POSIX and is reserved on Windows, so it's the
        // only delimiter we can pick that cannot clash with real filenames
        // (newline, tab, etc. are all legal on Linux).
        let path_list: Vec<PathBuf> = path_str
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();

        if path_list.is_empty() {
            return;
        }

        let Some(tx) = self.as_mut().rust_mut().tx.clone() else {
            self.as_mut().update_aux_properties();
            return;
        };

        // Drag-and-drop can mix files and folders. Expand directories
        // up-front with `collect_paths` so we know the final row count
        // *before* touching the store, which means we can call
        // `beginInsertRows` in the order Qt's model protocol requires.
        let mut expanded: Vec<PathBuf> = Vec::new();
        for p in path_list {
            if p.is_dir() {
                expanded.extend(collect_paths(&p, true));
            } else {
                expanded.push(p);
            }
        }
        self.as_mut().insert_expanded(&tx, expanded);
    }

    fn add_folder(mut self: Pin<&mut Self>, path: &QString) {
        self.as_mut().ensure_channel();
        let path = PathBuf::from(path.to_string());

        let Some(tx) = self.as_mut().rust_mut().tx.clone() else {
            self.as_mut().update_aux_properties();
            return;
        };

        let expanded = collect_paths(&path, true);
        self.as_mut().insert_expanded(&tx, expanded);
    }

    /// Insert a pre-expanded list of files, obeying Qt's model protocol:
    /// `beginInsertRows` → mutate the store → `endInsertRows`.
    fn insert_expanded(
        mut self: Pin<&mut Self>,
        tx: &Sender<FileStoreEvent>,
        expanded: Vec<PathBuf>,
    ) {
        if expanded.is_empty() {
            self.as_mut().update_aux_properties();
            return;
        }

        // Qt's model protocol passes `first` / `last` as `i32`, so both the
        // current row count and the delta must fit inside `i32` and their
        // sum (minus one) must not overflow. A silent wrap here would
        // feed `begin_insert_rows` a nonsense range, corrupting the model
        // cursor. Bail out cleanly on any of the failure modes instead.
        let store_len = self.rust().store.len();
        let Ok(start) = i32::try_from(store_len) else {
            log::error!("file model row count exceeds i32::MAX; refusing insert");
            self.as_mut().update_aux_properties();
            return;
        };
        let Ok(delta) = i32::try_from(expanded.len()) else {
            log::error!("drop size exceeds i32::MAX rows; refusing insert");
            self.as_mut().update_aux_properties();
            return;
        };
        let Some(end) = start.checked_add(delta).and_then(|e| e.checked_sub(1)) else {
            log::error!("row arithmetic would overflow i32; refusing insert");
            self.as_mut().update_aux_properties();
            return;
        };

        unsafe {
            self.as_mut()
                .begin_insert_rows(&QModelIndex::default(), start, end);
        }
        self.as_mut().rust_mut().store.add_files(expanded, tx);
        unsafe {
            self.as_mut().end_insert_rows();
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
        // Keep the detail drawer anchored to the same file across the
        // removal. If we removed the currently-viewed row, clear the
        // drawer; if we removed a row above it, shift the stored row
        // index down by one so it still points at the same file.
        let current_detail = self.rust().detail_row;
        if current_detail == index {
            self.as_mut().reset_detail();
        } else if current_detail > index && current_detail > 0 {
            self.as_mut().set_detail_row(current_detail - 1);
        }
        self.as_mut().update_aux_properties();
    }

    fn clear_files(mut self: Pin<&mut Self>) {
        unsafe {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().store.clear();
            self.as_mut().end_reset_model();
        }
        // The file backing the drawer snapshot is gone; wipe the drawer
        // so QML doesn't keep rendering from a stale `detail_items`.
        self.as_mut().reset_detail();
        self.as_mut().update_aux_properties();
    }

    fn clean_all(mut self: Pin<&mut Self>) {
        self.as_mut().ensure_channel();
        if let Some(tx) = self.as_mut().rust_mut().tx.clone() {
            self.as_mut().rust_mut().store.clean_files(&tx);
        }
        // Re-snapshot the drawer so it reflects the cleaned file's
        // (soon-to-be-empty) metadata instead of the stale pre-clean
        // values captured by the last `select_detail`.
        let current_detail = self.rust().detail_row;
        if current_detail >= 0 {
            self.as_mut().select_detail(current_detail);
        }
        self.as_mut().update_aux_properties();
    }

    /// Wipe the detail drawer snapshot. Called when the backing file is
    /// removed or the store is cleared.
    fn reset_detail(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().detail_items.clear();
        self.as_mut().set_detail_row(-1);
        self.as_mut().set_detail_group(QString::default());
        self.as_mut().set_detail_count(0);
        self.as_mut().set_detail_error(QString::default());
    }

    fn select_detail(mut self: Pin<&mut Self>, row: i32) {
        // Capture a snapshot of the row's metadata and error into the
        // detail_* fields so that the QML details drawer has something
        // stable to render. Passing row = -1 clears the details.
        let mut items: Vec<(String, String)> = Vec::new();
        let mut group = QString::default();
        let mut error = String::new();

        if row >= 0
            && let Ok(idx) = usize::try_from(row)
            && let Some(entry) = self.rust().store.get(idx)
        {
            if let Some(meta) = &entry.metadata {
                for g in &meta.groups {
                    // The QML layer renders a flat list; use the first
                    // group's filename as the header label and concatenate
                    // every group's items below it.
                    if group.is_empty() {
                        group = QString::from(&g.filename as &str);
                    }
                    for it in &g.items {
                        items.push((it.key.clone(), it.value.clone()));
                    }
                }
            }
            if let Some(err) = &entry.error {
                error.clone_from(err);
            }
        }

        let count = i32::try_from(items.len()).unwrap_or(i32::MAX);
        self.as_mut().rust_mut().detail_items = items;
        self.as_mut().set_detail_row(row);
        self.as_mut().set_detail_group(group);
        self.as_mut().set_detail_count(count);
        self.as_mut()
            .set_detail_error(QString::from(&error as &str));
    }

    fn detail_key(&self, index: i32) -> QString {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.rust().detail_items.get(i))
            .map_or_else(QString::default, |(k, _)| QString::from(k as &str))
    }

    fn detail_value(&self, index: i32) -> QString {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.rust().detail_items.get(i))
            .map_or_else(QString::default, |(_, v)| QString::from(v as &str))
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
