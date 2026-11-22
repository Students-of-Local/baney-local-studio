use gtk::prelude::*;
use gtk::{gdk, glib};

pub const ICON: &str = "dev.localstudio.LocalStudio";
const MARK: &[u8] = include_bytes!("../assets/mark.svg");

pub fn mark(size: i32) -> Option<gtk::Picture> {
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_static(MARK)).ok()?;
    let picture = gtk::Picture::for_paintable(&texture);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_size_request(size, size);
    picture.set_halign(gtk::Align::Center);
    picture.set_accessible_role(gtk::AccessibleRole::Presentation);
    Some(picture)
}

pub fn install_icon() {
    let dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("share").join("icons")));
    if let (Some(display), Some(dir)) = (gdk::Display::default(), dir.filter(|dir| dir.is_dir())) {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
    gtk::Window::set_default_icon_name(ICON);
}
