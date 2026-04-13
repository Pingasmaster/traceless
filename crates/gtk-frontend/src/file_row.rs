use gtk::prelude::*;

use crate::badge;
use traceless_core::FileState;

/// Walk up from `widget` to the enclosing `gtk::ListBoxRow` and return its
/// current index, or `None` if there is no such ancestor.
fn row_index_of(widget: &impl IsA<gtk::Widget>) -> Option<usize> {
    let row = widget
        .ancestor(gtk::ListBoxRow::static_type())
        .and_then(|w| w.downcast::<gtk::ListBoxRow>().ok())?;
    usize::try_from(row.index()).ok()
}

/// Create a file row driven by a `ListBox::bind_model` binding: click
/// callbacks receive the *current* row position at fire-time, looked up via
/// the enclosing `ListBoxRow`. Safe to use after rows have been removed or
/// reordered.
pub fn create_file_row_bound(
    filename: &str,
    directory: &str,
    state: FileState,
    metadata_count: usize,
    on_remove: impl Fn(usize) + 'static,
    on_details: impl Fn(usize) + 'static,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);

    let remove_btn = gtk::Button::from_icon_name("edit-delete-symbolic");
    remove_btn.add_css_class("flat");
    remove_btn.add_css_class("remove");
    remove_btn.set_tooltip_text(Some("Remove file"));
    remove_btn.connect_clicked(move |btn| {
        if let Some(idx) = row_index_of(btn) {
            on_remove(idx);
        }
    });

    let sep = gtk::Separator::new(gtk::Orientation::Vertical);

    let file_btn = gtk::Button::new();
    file_btn.add_css_class("flat");
    file_btn.add_css_class("file");
    file_btn.set_hexpand(true);

    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    inner.set_margin_start(8);
    inner.set_margin_end(8);
    inner.set_margin_top(8);
    inner.set_margin_bottom(8);

    let icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
    icon.set_pixel_size(32);
    inner.append(&icon);

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

    let badge_widget = match state.simple_state() {
        "working" => badge::spinner_badge(),
        "error" => badge::error_badge(),
        "warning" => badge::warning_badge(),
        "has-metadata" => badge::metadata_count_badge(metadata_count),
        "clean" => badge::success_badge(),
        _ => gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast(),
    };
    inner.append(&badge_widget);

    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
    arrow.add_css_class("dim-label");
    inner.append(&arrow);

    file_btn.set_child(Some(&inner));
    file_btn.connect_clicked(move |btn| {
        if let Some(idx) = row_index_of(btn) {
            on_details(idx);
        }
    });

    row.append(&remove_btn);
    row.append(&sep);
    row.append(&file_btn);

    row
}

