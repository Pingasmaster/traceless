use gtk::prelude::*;

/// Create a colored pill badge widget.
pub fn create_badge(text: &str, css_class: &str) -> gtk::Widget {
    let label = gtk::Label::new(Some(text));
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&label));
    frame.add_css_class("badge");
    frame.add_css_class(css_class);
    frame.set_halign(gtk::Align::Center);
    frame.set_valign(gtk::Align::Center);
    frame.upcast()
}

/// Create a badge showing a metadata count (purple).
pub fn metadata_count_badge(count: usize) -> gtk::Widget {
    create_badge(&count.to_string(), "metadata")
}

/// Create a success (green checkmark) badge.
pub fn success_badge() -> gtk::Widget {
    let icon = gtk::Image::from_icon_name("emblem-ok-symbolic");
    icon.set_pixel_size(16);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&icon));
    frame.add_css_class("badge");
    frame.add_css_class("success");
    frame.set_halign(gtk::Align::Center);
    frame.set_valign(gtk::Align::Center);
    frame.upcast()
}

/// Create a warning badge.
pub fn warning_badge() -> gtk::Widget {
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    icon.set_pixel_size(16);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&icon));
    frame.add_css_class("badge");
    frame.add_css_class("warning");
    frame.set_halign(gtk::Align::Center);
    frame.set_valign(gtk::Align::Center);
    frame.upcast()
}

/// Create an error badge.
pub fn error_badge() -> gtk::Widget {
    let icon = gtk::Image::from_icon_name("dialog-error-symbolic");
    icon.set_pixel_size(16);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&icon));
    frame.add_css_class("badge");
    frame.add_css_class("error");
    frame.set_halign(gtk::Align::Center);
    frame.set_valign(gtk::Align::Center);
    frame.upcast()
}

/// Create a spinner badge for "working" state.
pub fn spinner_badge() -> gtk::Widget {
    let spinner = adw::Spinner::new();
    spinner.set_halign(gtk::Align::Center);
    spinner.set_valign(gtk::Align::Center);
    spinner.set_size_request(22, 22);
    spinner.upcast()
}
