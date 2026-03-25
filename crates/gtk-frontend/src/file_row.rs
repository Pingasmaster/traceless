use gtk::prelude::*;

use crate::badge;
use traceless_core::FileState;

/// Create a file row widget matching the original app's layout:
/// [X remove] | [icon] [filename / directory] [badge] [>]
pub fn create_file_row(
    filename: &str,
    directory: &str,
    state: FileState,
    metadata_count: usize,
    index: usize,
    on_remove: impl Fn(usize) + 'static,
    on_details: impl Fn(usize) + 'static,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);

    // Remove button
    let remove_btn = gtk::Button::from_icon_name("edit-delete-symbolic");
    remove_btn.add_css_class("flat");
    remove_btn.add_css_class("remove");
    remove_btn.set_tooltip_text(Some("Remove file"));
    remove_btn.connect_clicked(move |_| on_remove(index));

    // Separator
    let sep = gtk::Separator::new(gtk::Orientation::Vertical);

    // File button (clickable area for the rest of the row)
    let file_btn = gtk::Button::new();
    file_btn.add_css_class("flat");
    file_btn.add_css_class("file");
    file_btn.set_hexpand(true);

    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    inner.set_margin_start(8);
    inner.set_margin_end(8);
    inner.set_margin_top(8);
    inner.set_margin_bottom(8);

    // File icon
    let icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
    icon.set_pixel_size(32);
    inner.append(&icon);

    // Name + directory
    let text_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text_box.set_hexpand(true);
    text_box.set_halign(gtk::Align::Start);
    text_box.set_valign(gtk::Align::Center);

    let name_label = gtk::Label::new(Some(filename));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    text_box.append(&name_label);

    if !directory.is_empty() {
        let dir_label = gtk::Label::new(Some(directory));
        dir_label.set_halign(gtk::Align::Start);
        dir_label.add_css_class("dim-label");
        dir_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        dir_label.set_css_classes(&["dim-label", "caption"]);
        text_box.append(&dir_label);
    }

    inner.append(&text_box);

    // Status badge
    let badge_widget = match state.simple_state() {
        "working" => badge::spinner_badge(),
        "error" => badge::error_badge(),
        "warning" => badge::warning_badge(),
        "has-metadata" => badge::metadata_count_badge(metadata_count),
        "clean" => badge::success_badge(),
        _ => gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast(),
    };
    inner.append(&badge_widget);

    // Navigation arrow
    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
    arrow.add_css_class("dim-label");
    inner.append(&arrow);

    file_btn.set_child(Some(&inner));
    file_btn.connect_clicked(move |_| on_details(index));

    row.append(&remove_btn);
    row.append(&sep);
    row.append(&file_btn);

    row
}
