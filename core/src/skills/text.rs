pub(super) fn line(text: &str) -> String {
    let words = text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|word| !word.is_empty());
    words.collect::<Vec<_>>().join(" ")
}

#[must_use]
pub fn plain(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}
