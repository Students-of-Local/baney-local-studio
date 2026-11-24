const SCHEMES: &[&str] = &["http", "https", "mailto"];

fn hidden(char: char) -> bool {
    char.is_control() || matches!(char, '\u{200E}' | '\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn refused_link(url: &str) -> Option<String> {
    let scheme = url.split_once(':').map_or("", |(scheme, _)| scheme);
    if SCHEMES.iter().any(|known| scheme.eq_ignore_ascii_case(known)) {
        return None;
    }
    let shown: String = url.chars().filter(|char| !hidden(*char)).take(200).collect();
    Some(format!("Only web and mail links open from a thread, so this one was not opened: {shown}"))
}
