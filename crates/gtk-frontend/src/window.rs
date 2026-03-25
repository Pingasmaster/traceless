use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

use adw::prelude::*;

use crate::bridge;
use crate::details_view::DetailsView;
use crate::dialogs;
use crate::empty_view;
use crate::files_view::FilesView;
use traceless_core::{FileStore, FileStoreEvent};

pub struct Window;

impl Window {
    pub fn build(app: &adw::Application) -> adw::ApplicationWindow {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Traceless")
            .default_width(500)
            .default_height(700)
            .build();
        window.add_css_class("main");

        // Shared state
        let store = Rc::new(RefCell::new(FileStore::new()));
        let tx: Rc<RefCell<Option<mpsc::Sender<FileStoreEvent>>>> =
            Rc::new(RefCell::new(None));
        let show_warning = Rc::new(RefCell::new(true));

        // --- Layout ---

        // Main content box
        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Header bar
        let header = adw::HeaderBar::new();

        let title_widget = adw::WindowTitle::new("Traceless", "");
        header.set_title_widget(Some(&title_widget));

        // Add Files button
        let add_files_btn = gtk::Button::with_label("Add Files");
        add_files_btn.set_tooltip_text(Some("Add files (Ctrl+O)"));

        // Add Folders button
        let add_folders_btn = gtk::Button::with_label("Add Folders");
        add_folders_btn.set_tooltip_text(Some("Add folder"));

        header.pack_start(&add_files_btn);
        header.pack_start(&add_folders_btn);

        // Menu button
        let menu = gtk::gio::Menu::new();
        menu.append(Some("New Window"), Some("app.new-window"));
        menu.append(Some("Clear Window"), Some("win.clear-files"));
        let section2 = gtk::gio::Menu::new();
        section2.append(Some("About Traceless"), Some("win.about"));
        menu.append_section(None, &section2);

        let menu_btn = gtk::MenuButton::new();
        menu_btn.set_icon_name("open-menu-symbolic");
        menu_btn.set_menu_model(Some(&menu));
        header.pack_end(&menu_btn);

        main_box.append(&header);

        // View stack: empty vs files
        let view_stack = gtk::Stack::new();
        view_stack.set_vexpand(true);
        view_stack.set_transition_type(gtk::StackTransitionType::Crossfade);

        let empty_view = empty_view::create_empty_view();
        view_stack.add_named(&empty_view, Some("empty"));

        let files_view = Rc::new(FilesView::new());
        view_stack.add_named(&files_view.widget, Some("files"));

        view_stack.set_visible_child_name("empty");
        main_box.append(&view_stack);

        // Details side panel (Flap-like using a Box + Revealer)
        let content_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content_box.append(&main_box);

        let details_revealer = gtk::Revealer::new();
        details_revealer.set_transition_type(gtk::RevealerTransitionType::SlideLeft);
        details_revealer.set_reveal_child(false);

        let sep = gtk::Separator::new(gtk::Orientation::Vertical);
        let details_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        details_box.append(&sep);

        let details_revealer_clone = details_revealer.clone();
        let details_view = Rc::new(DetailsView::new(move || {
            details_revealer_clone.set_reveal_child(false);
        }));
        details_view.widget.set_width_request(350);
        details_box.append(&details_view.widget);

        details_revealer.set_child(Some(&details_box));
        content_box.append(&details_revealer);

        window.set_content(Some(&content_box));

        // --- Event handling setup ---
        let (event_tx, event_rx) = mpsc::channel::<FileStoreEvent>();
        *tx.borrow_mut() = Some(event_tx.clone());

        // Install event pump from worker threads -> GTK main loop
        {
            let store = store.clone();
            let files_view = files_view.clone();
            let view_stack = view_stack.clone();
            let details_revealer = details_revealer.clone();

            let rebuild = {
                let store = store.clone();
                let files_view = files_view.clone();
                let details_revealer = details_revealer.clone();
                Rc::new(move || {
                    let s = store.borrow();
                    let dr = details_revealer.clone();
                    let store2 = store.clone();
                    files_view.rebuild_list(
                        &s,
                        {
                            let store3 = store2.clone();
                            let files_view2 = files_view.clone();
                            let vs = view_stack.clone();
                            let dr2 = dr.clone();
                            move |idx| {
                                store3.borrow_mut().remove_file(idx);
                                let s = store3.borrow();
                                if s.is_empty() {
                                    vs.set_visible_child_name("empty");
                                    dr2.set_reveal_child(false);
                                }
                                files_view2.rebuild_list(
                                    &s,
                                    move |_| {},
                                    move |_| {},
                                );
                            }
                        },
                        {
                            let store3 = store2.clone();
                            let dv = details_view.clone();
                            let dr2 = dr.clone();
                            move |idx| {
                                let s = store3.borrow();
                                if let Some(entry) = s.get(idx) {
                                    dv.show_file(entry);
                                    dr2.set_reveal_child(true);
                                }
                            }
                        },
                    );

                    // Update status
                    let cleaned = s.cleaned_count();
                    let total = s.len();
                    if !s.has_working() && cleaned > 0 {
                        files_view
                            .status
                            .set_done(&format!("{cleaned} file{} cleaned.", if cleaned == 1 { "" } else { "s" }));
                    } else if s.has_working() {
                        let done = s.files().iter().filter(|f| !f.state.is_working()).count();
                        let frac = if total > 0 { done as f64 / total as f64 } else { 0.0 };
                        files_view.status.set_working(frac);
                    } else {
                        files_view.status.set_idle();
                    }
                })
            };

            bridge::install_event_pump(event_rx, move |event| {
                store.borrow_mut().apply_event(&event);
                rebuild();
            });
        }

        // --- Connect Actions ---

        // Lightweight cleaning toggle
        {
            let store = store.clone();
            files_view.settings_switch.connect_state_set(move |_, active| {
                store.borrow_mut().lightweight_mode = active;
                gtk::glib::Propagation::Proceed
            });
        }

        // Add Files button
        {
            let window_clone = window.clone();
            let store = store.clone();
            let tx = tx.clone();
            let view_stack = view_stack.clone();
            add_files_btn.connect_clicked(move |_| {
                let store = store.clone();
                let tx = tx.clone();
                let vs = view_stack.clone();
                dialogs::show_file_chooser(&window_clone, move |paths| {
                    if !paths.is_empty()
                        && let Some(sender) = tx.borrow().as_ref()
                    {
                        store.borrow_mut().add_files(paths, sender.clone());
                        vs.set_visible_child_name("files");
                    }
                });
            });
        }

        // Add Folders button
        {
            let window_clone = window.clone();
            let store = store.clone();
            let tx = tx.clone();
            let view_stack = view_stack.clone();
            add_folders_btn.connect_clicked(move |_| {
                let store = store.clone();
                let tx = tx.clone();
                let vs = view_stack.clone();
                dialogs::show_folder_chooser(&window_clone, move |path| {
                    if let Some(sender) = tx.borrow().as_ref() {
                        store
                            .borrow_mut()
                            .add_directory(&path, true, sender.clone());
                        vs.set_visible_child_name("files");
                    }
                });
            });
        }

        // Clean button
        {
            let window_clone = window.clone();
            let store = store.clone();
            let tx = tx.clone();
            let show_warning = show_warning.clone();
            files_view.clean_button.connect_clicked(move |_| {
                let store = store.clone();
                let tx = tx.clone();
                let sw = show_warning.clone();

                if *sw.borrow() {
                    let store2 = store.clone();
                    let tx2 = tx.clone();
                    dialogs::show_cleaning_warning(&window_clone, move |confirmed| {
                        if confirmed
                            && let Some(sender) = tx2.borrow().as_ref()
                        {
                            store2.borrow_mut().clean_files(sender.clone());
                        }
                    });
                } else if let Some(sender) = tx.borrow().as_ref() {
                    store.borrow_mut().clean_files(sender.clone());
                }
            });
        }

        // Window actions
        let clear_action = gtk::gio::SimpleAction::new("clear-files", None);
        {
            let store = store.clone();
            let view_stack = view_stack.clone();
            let details_revealer = details_revealer.clone();
            clear_action.connect_activate(move |_, _| {
                store.borrow_mut().clear();
                view_stack.set_visible_child_name("empty");
                details_revealer.set_reveal_child(false);
            });
        }
        window.add_action(&clear_action);

        let about_action = gtk::gio::SimpleAction::new("about", None);
        {
            let window_clone = window.clone();
            about_action.connect_activate(move |_, _| {
                dialogs::show_about_dialog(&window_clone);
            });
        }
        window.add_action(&about_action);

        window
    }
}
