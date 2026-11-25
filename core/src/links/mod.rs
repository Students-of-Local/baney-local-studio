pub(crate) mod studio;

const SCHEMES: &[&str] = &["http", "https", "mailto"];

fn hidden(char: char) -> bool {
    char.is_control()
        || matches!(char, '\u{AD}' | '\u{61C}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}')
        || matches!(char, '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}')
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
