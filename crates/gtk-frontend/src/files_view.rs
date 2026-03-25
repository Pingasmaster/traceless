use adw::prelude::*;

use crate::file_row;
use crate::settings_popover;
use crate::status_indicator::StatusIndicator;
use traceless_core::FileStore;

/// The files list view with toolbar at the bottom.
pub struct FilesView {
    pub widget: gtk::Box,
    pub list_box: gtk::ListBox,
    pub status: StatusIndicator,
    pub settings_switch: gtk::Switch,
    pub clean_button: gtk::Button,
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

        let (settings_btn, settings_switch) = settings_popover::create_settings_popover();
        toolbar.append(&settings_btn);

        let clean_button = gtk::Button::with_label("Clean");
        clean_button.add_css_class("destructive-action");
        clean_button.set_sensitive(false);
        toolbar.append(&clean_button);

        outer.append(&toolbar);

        Self {
            widget: outer,
            list_box,
            status,
            settings_switch,
            clean_button,
        }
    }

    /// Rebuild the file list from the store.
    #[allow(clippy::redundant_closure)]
    pub fn rebuild_list(
        &self,
        store: &FileStore,
        on_remove: impl Fn(usize) + Clone + 'static,
        on_details: impl Fn(usize) + Clone + 'static,
    ) {
        // Clear existing rows
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }

        for (i, entry) in store.files().iter().enumerate() {
            let remove_cb = on_remove.clone();
            let details_cb = on_details.clone();
            let row = file_row::create_file_row(
                &entry.filename,
                &entry.directory,
                entry.state,
                entry.total_metadata(),
                i,
                move |idx| remove_cb(idx),
                move |idx| details_cb(idx),
            );
            self.list_box.append(&row);
        }

        // Update clean button sensitivity
        self.clean_button
            .set_sensitive(store.cleanable_count() > 0 && !store.has_working());
    }
}
