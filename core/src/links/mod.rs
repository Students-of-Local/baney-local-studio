pub(crate) mod open;
pub(crate) mod studio;

const SCHEMES: &[&str] = &["http", "https", "mailto"];

fn hidden(char: char) -> bool {
    char.is_control()
        || matches!(char, '\u{AD}' | '\u{34F}' | '\u{61C}' | '\u{115F}'..='\u{1160}' | '\u{17B4}'..='\u{17B5}')
        || matches!(char, '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}')
        || matches!(char, '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFA0}')
        || matches!(char, '\u{FFF0}'..='\u{FFF8}' | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}')
        || matches!(char, '\u{E0000}'..='\u{E0FFF}')
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
