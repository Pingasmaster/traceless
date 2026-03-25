use gtk::prelude::*;

/// Status indicator widget that shows idle / progress / done states.
pub struct StatusIndicator {
    pub widget: gtk::Stack,
    _idle_label: gtk::Label,
    _progress_box: gtk::Box,
    progress_bar: gtk::ProgressBar,
    done_label: gtk::Label,
}

impl StatusIndicator {
    pub fn new() -> Self {
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_halign(gtk::Align::Start);

        // Idle state
        let idle_label = gtk::Label::new(None);
        stack.add_named(&idle_label, Some("idle"));

        // Working state: progress bar
        let progress_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let progress_bar = gtk::ProgressBar::new();
        progress_bar.set_hexpand(true);
        progress_bar.set_valign(gtk::Align::Center);
        progress_box.append(&progress_bar);
        stack.add_named(&progress_box, Some("working"));

        // Done state
        let done_label = gtk::Label::new(None);
        stack.add_named(&done_label, Some("done"));

        stack.set_visible_child_name("idle");

        Self {
            widget: stack,
            _idle_label: idle_label,
            _progress_box: progress_box,
            progress_bar,
            done_label,
        }
    }

    pub fn set_idle(&self) {
        self.widget.set_visible_child_name("idle");
    }

    pub fn set_working(&self, fraction: f64) {
        self.progress_bar.set_fraction(fraction);
        self.widget.set_visible_child_name("working");
    }

    pub fn set_done(&self, message: &str) {
        self.done_label.set_text(message);
        self.widget.set_visible_child_name("done");
    }
}
