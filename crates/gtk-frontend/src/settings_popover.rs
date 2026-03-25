use gtk::prelude::*;

/// Create the settings popover with a lightweight cleaning toggle.
pub fn create_settings_popover() -> (gtk::MenuButton, gtk::Switch) {
    let switch = gtk::Switch::new();
    switch.set_valign(gtk::Align::Center);

    let label = gtk::Label::new(Some("Lightweight Cleaning"));

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_margin_top(8);
    row.set_margin_bottom(8);
    row.append(&label);
    row.append(&switch);

    let popover = gtk::Popover::new();
    popover.set_child(Some(&row));

    let button = gtk::MenuButton::new();
    button.set_icon_name("emblem-system-symbolic");
    button.set_popover(Some(&popover));
    button.set_tooltip_text(Some("Settings"));

    (button, switch)
}
