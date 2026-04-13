const CSS: &str = r"
@define-color accent_color #813d9c;
@define-color accent_bg_color @accent_color;
@define-color accent_fg_color #fff;

window.main {
    background: linear-gradient(180deg, @accent_bg_color 0px, @accent_bg_color 20px, @theme_bg_color 20px);
}

window.main headerbar {
    background-color: @accent_bg_color;
    color: @accent_fg_color;
}

listview.files > row {
    padding: 0;
    border-bottom: 1px solid @borders;
}

listview.files .remove,
listview.files .file {
    border-radius: unset;
    font-weight: unset;
}

listview.files .remove {
    padding: 6px 12px;
}

listview.files separator {
    opacity: 0;
    transition: 0.1s opacity;
}

listview.files > row:hover separator {
    opacity: 1;
    transition: none;
}

.badge {
    border-radius: 11px;
    background-color: rgba(0, 0, 0, 0.5);
    color: #fff;
    font-weight: bold;
    font-size: 12px;
    min-width: 22px;
    min-height: 22px;
}

.badge label {
    margin: 0 6px;
    padding: 0 0.4em;
}

.badge.metadata {
    background-color: @accent_color;
}

.badge.warning {
    background-color: @warning_color;
}

.badge.error {
    background-color: @error_color;
}

.badge.success {
    background-color: @success_color;
}

.toolbar.details {
    background-color: @headerbar_bg_color;
    color: @headerbar_fg_color;
}

listview.metadata {
    background: none;
}

listview.metadata > row {
    margin: 12px 24px;
}

row.metadata > box {
    margin: 6px;
    border-spacing: 12px;
}
";

pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("Could not get default display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
