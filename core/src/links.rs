const SCHEMES: &[&str] = &["http", "https", "mailto"];

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn refused_link(url: &str) -> Option<String> {
    let scheme = url.split_once(':').map_or("", |(scheme, _)| scheme);
    if SCHEMES.iter().any(|known| scheme.eq_ignore_ascii_case(known)) {
        return None;
    }
    let shown: String = url.chars().filter(|char| !char.is_control()).take(200).collect();
    Some(format!("Only web and mail links open from a thread, so this one was not opened: {shown}"))
}
