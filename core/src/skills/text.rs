pub(super) fn key(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

pub(super) fn line(text: &str) -> String {
    let words = text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|word| !word.is_empty());
    words.collect::<Vec<_>>().join(" ")
}
