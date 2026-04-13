use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib::BoxedAnyObject;

use crate::file_row;
use crate::status_indicator::StatusIndicator;
use traceless_core::{FileEntry, FileState, FileStore};

/// Display snapshot of a `FileEntry` suitable for rendering one list row.
#[derive(Clone)]
pub struct FileRowData {
    pub filename: String,
    pub directory: String,
    pub state: FileState,
    pub metadata_count: usize,
}

impl FileRowData {
    fn from_entry(entry: &FileEntry) -> Self {
        Self {
            filename: entry.filename.clone(),
            directory: entry.directory.clone(),
            state: entry.state,
            metadata_count: entry.total_metadata(),
        }
    }
}

type RowCallback = Rc<RefCell<Option<Rc<dyn Fn(usize)>>>>;

/// The files list view with toolbar at the bottom.
pub struct FilesView {
    pub widget: gtk::Box,
    pub list_store: gtk::gio::ListStore,
    pub status: StatusIndicator,
    pub clean_button: gtk::Button,
    on_remove: RowCallback,
    on_details: RowCallback,
}

impl FilesView {
    pub fn new() -> Self {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Scrollable file list
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_vexpand(true);
        scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);

        let list_box = gtk::ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::None);
        list_box.add_css_class("files");
        scrolled.set_child(Some(&list_box));
        outer.append(&scrolled);

        let list_store = gtk::gio::ListStore::new::<BoxedAnyObject>();
        let on_remove: RowCallback = Rc::new(RefCell::new(None));
        let on_details: RowCallback = Rc::new(RefCell::new(None));

        // Bind the ListBox to the ListStore. Each item is a BoxedAnyObject
        // wrapping a FileRowData snapshot. The click callbacks walk up to the
        // containing ListBoxRow at fire-time, so they always see the current
        // position even after rows shift due to removals.
        {
            let on_remove = on_remove.clone();
            let on_details = on_details.clone();
            list_box.bind_model(Some(&list_store), move |obj| {
                let boxed = obj
                    .downcast_ref::<BoxedAnyObject>()
                    .expect("list store holds BoxedAnyObject");
                let data = boxed.borrow::<FileRowData>();
                let on_remove = on_remove.clone();
                let on_details = on_details.clone();
                file_row::create_file_row_bound(
                    &data.filename,
                    &data.directory,
                    data.state,
                    data.metadata_count,
                    move |row| {
                        if let Some(cb) = on_remove.borrow().as_ref() {
                            cb(row);
                        }
                    },
                    move |row| {
                        if let Some(cb) = on_details.borrow().as_ref() {
                            cb(row);
                        }
                    },
                )
                .upcast()
            });
        }

        // Separator
        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        outer.append(&sep);

        // Bottom toolbar
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        toolbar.set_margin_start(8);
        toolbar.set_margin_end(8);
        toolbar.set_margin_top(6);
        toolbar.set_margin_bottom(6);

        let status = StatusIndicator::new();
        toolbar.append(&status.widget);

        let clean_button = gtk::Button::with_label("Clean");
        clean_button.add_css_class("destructive-action");
        clean_button.set_sensitive(false);
        toolbar.append(&clean_button);

        outer.append(&toolbar);

        Self {
            widget: outer,
            list_store,
            status,
            clean_button,
            on_remove,
            on_details,
        }
    }

    /// Install the row click callbacks. Each callback receives the *current*
    /// row index at click time — safe to call after rows have been removed.
    pub fn bind_callbacks(
        &self,
        on_remove: impl Fn(usize) + 'static,
        on_details: impl Fn(usize) + 'static,
    ) {
        *self.on_remove.borrow_mut() = Some(Rc::new(on_remove));
        *self.on_details.borrow_mut() = Some(Rc::new(on_details));
    }

    /// Append freshly added entries in the range `[start, store.len())`.
    pub fn append_new(&self, store: &FileStore, start: usize) {
        let items: Vec<BoxedAnyObject> = store.files()[start..]
            .iter()
            .map(|entry| BoxedAnyObject::new(FileRowData::from_entry(entry)))
            .collect();
        self.list_store
            .splice(self.list_store.n_items(), 0, &items);
        self.refresh_clean_button(store);
    }

    /// Replace a single row in place (state/metadata update for one entry).
    pub fn update_row(&self, store: &FileStore, index: usize) {
        let Some(entry) = store.get(index) else {
            return;
        };
        let Ok(pos) = u32::try_from(index) else {
            return;
        };
        let new_item = BoxedAnyObject::new(FileRowData::from_entry(entry));
        self.list_store.splice(pos, 1, &[new_item]);
        self.refresh_clean_button(store);
    }

    /// Remove a single row.
    pub fn remove_row(&self, store: &FileStore, index: usize) {
        let Ok(pos) = u32::try_from(index) else {
            return;
        };
        if pos < self.list_store.n_items() {
            self.list_store.remove(pos);
        }
        self.refresh_clean_button(store);
    }

    pub fn clear_rows(&self) {
        self.list_store.remove_all();
        self.clean_button.set_sensitive(false);
    }

    fn refresh_clean_button(&self, store: &FileStore) {
        self.clean_button
            .set_sensitive(store.cleanable_count() > 0 && !store.has_working());
    }
}
