use gtk::prelude::*;

/// Create a metadata key-value row for the details view.
pub fn create_metadata_row(key: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_margin_start(6);
    row.set_margin_end(6);
    row.set_margin_top(3);
    row.set_margin_bottom(3);

    let key_label = gtk::Label::new(Some(key));
    key_label.set_halign(gtk::Align::End);
    key_label.set_valign(gtk::Align::Start);
    key_label.set_width_chars(20);
    key_label.set_xalign(1.0);
    key_label.add_css_class("dim-label");
    key_label.set_wrap(true);
    key_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);

    let value_label = gtk::Label::new(Some(value));
    value_label.set_halign(gtk::Align::Start);
    value_label.set_hexpand(true);
    value_label.set_selectable(true);
    value_label.set_wrap(true);
    value_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);

    row.append(&key_label);
    row.append(&value_label);

    row
}
