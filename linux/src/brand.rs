use gtk::prelude::*;
use gtk::{gdk, glib};

pub const ICON: &str = "dev.localstudio.LocalStudio";
const MARK: &[u8] = include_bytes!("../assets/mark.svg");

pub fn mark(size: i32) -> Option<gtk::Image> {
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_static(MARK)).ok()?;
    let image = gtk::Image::from_paintable(Some(&texture));
    image.set_pixel_size(size);
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    image.set_hexpand(false);
    image.set_vexpand(false);
    image.set_accessible_role(gtk::AccessibleRole::Presentation);
    Some(image)
}

pub fn install_icon() {
    let dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("share").join("icons")));
    if let (Some(display), Some(dir)) = (gdk::Display::default(), dir.filter(|dir| dir.is_dir())) {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
    gtk::Window::set_default_icon_name(ICON);
}
