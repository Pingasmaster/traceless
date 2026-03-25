use adw::prelude::*;

use crate::metadata_row;
use traceless_core::{FileEntry, MetadataSet};

/// Create the details view panel shown on the right side.
pub struct DetailsView {
    pub widget: gtk::Box,
    content: gtk::Box,
}

impl DetailsView {
    pub fn new(on_back: impl Fn() + 'static) -> Self {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Header bar with "Details" title and back button
        let header = adw::HeaderBar::new();
        header.set_show_end_title_buttons(false);
        header.set_show_start_title_buttons(false);

        let title = adw::WindowTitle::new("Details", "");
        header.set_title_widget(Some(&title));

        let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
        back_btn.set_tooltip_text(Some("Back"));
        back_btn.connect_clicked(move |_| on_back());
        header.pack_start(&back_btn);

        header.add_css_class("toolbar");
        header.add_css_class("details");
        outer.append(&header);

        // Scrollable content area
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_vexpand(true);
        scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        scrolled.set_child(Some(&content));

        outer.append(&scrolled);

        Self {
            widget: outer,
            content,
        }
    }

    /// Display metadata for the given file entry.
    pub fn show_file(&self, entry: &FileEntry) {
        // Clear existing content
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }

        if let Some(ref metadata) = entry.metadata {
            self.show_metadata(&entry.filename, metadata);
        } else if let Some(ref error) = entry.error {
            let status = adw::StatusPage::new();
            status.set_icon_name(Some("dialog-error-symbolic"));
            status.set_title("Error");
            status.set_description(Some(error));
            self.content.append(&status);
        } else {
            let status = adw::StatusPage::new();
            status.set_icon_name(Some("emblem-ok-symbolic"));
            status.set_title("No metadata found");
            self.content.append(&status);
        }
    }

    fn show_metadata(&self, _filename: &str, metadata: &MetadataSet) {
        for group in &metadata.groups {
            // Group heading
            let heading = gtk::Label::new(Some(&group.filename));
            heading.set_halign(gtk::Align::Start);
            heading.add_css_class("heading");
            heading.set_margin_top(6);
            self.content.append(&heading);

            // Key-value list in a boxed list
            let listbox = gtk::ListBox::new();
            listbox.set_selection_mode(gtk::SelectionMode::None);
            listbox.add_css_class("boxed-list");

            for item in &group.items {
                let row_widget = metadata_row::create_metadata_row(&item.key, &item.value);
                let list_row = gtk::ListBoxRow::new();
                list_row.set_child(Some(&row_widget));
                list_row.set_activatable(false);
                listbox.append(&list_row);
            }

            self.content.append(&listbox);
        }
    }
}
