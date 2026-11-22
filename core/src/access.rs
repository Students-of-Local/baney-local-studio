use crate::model::AccessMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ModelTier {
    Frontier,
    Small,
    Unknown,
}

impl ModelTier {
    #[must_use]
    pub fn access(self) -> AccessMode {
        match self {
            Self::Frontier => AccessMode::FullAccess,
            Self::Small => AccessMode::Auto,
            Self::Unknown => AccessMode::Supervised,
        }
    }
}

const SMALL_MARKERS: [&str; 20] = [
    "haiku",
    "mini",
    "nano",
    "lite",
    "oss",
    "small",
    "tiny",
    "gemma",
    "phi",
    "smol",
    "smollm",
    "tinyllama",
    "llama",
    "codellama",
    "mistral",
    "ministral",
    "mixtral",
    "devstral",
    "codestral",
    "granite",
];

const ALIASES: [&str; 3] = ["opus", "sonnet", "fable"];

const FAMILIES: [(&[&str], (u32, u32)); 11] = [
    (&["opus"], (4, 5)),
    (&["sonnet"], (4, 6)),
    (&["fable"], (0, 0)),
    (&["gpt"], (5, 2)),
    (&["gemini"], (3, 0)),
    (&["grok"], (4, 5)),
    (&["deepseek", "v"], (4, 0)),
    (&["kimi", "k"], (2, 6)),
    (&["glm"], (5, 3)),
    (&["minimax", "m"], (3, 0)),
    (&["qwen"], (3, 8)),
];

struct Piece {
    text: String,
    token: usize,
    first: bool,
}

fn pieces(model: &str) -> Vec<Piece> {
    let lowered = model.to_lowercase();
    let name = lowered.rsplit('/').next().unwrap_or_default();
    let name = name.split('[').next().unwrap_or_default();
    let mut out: Vec<Piece> = Vec::new();
    for (token, part) in name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|part| !part.is_empty()).enumerate() {
        let mut current = String::new();
        let mut first = true;
        for c in part.chars() {
            if !current.is_empty() && current.chars().all(|d| d.is_ascii_digit()) != c.is_ascii_digit() {
                out.push(Piece { text: std::mem::take(&mut current), token, first });
                first = false;
            }
            current.push(c);
        }
        if !current.is_empty() {
            out.push(Piece { text: current, token, first });
        }
    }
    out
}

fn number(piece: &Piece) -> Option<u32> {
    if piece.text.len() > 3 {
        return None;
    }
    piece.text.parse().ok()
}

fn billions(pieces: &[Piece]) -> Option<u32> {
    let mut best = None;
    for (index, piece) in pieces.iter().enumerate() {
        if !piece.first {
            continue;
        }
        let Some(count) = number(piece) else { continue };
        let rest: Vec<&Piece> = pieces[index + 1..].iter().take_while(|next| next.token == piece.token).collect();
        let size = match rest.as_slice() {
            [unit] if unit.text == "b" => Some(count),
            [x, experts, unit] if x.text == "x" && unit.text == "b" => number(experts).map(|each| count * each),
            _ => None,
        };
        if let Some(size) = size {
            best = Some(best.map_or(size, |seen: u32| seen.max(size)));
        }
    }
    best
}

fn digits(piece: &Piece) -> bool {
    piece.text.chars().all(|c| c.is_ascii_digit())
}

fn version<'a>(mut parts: impl Iterator<Item = &'a Piece>) -> Option<(u32, u32)> {
    let major = number(parts.next()?)?;
    Some((major, parts.next().and_then(number).unwrap_or(0)))
}

enum Found {
    Version((u32, u32)),
    Bare,
}

fn family_version(pieces: &[Piece], family: &[&str]) -> Option<Found> {
    let start = (0..pieces.len()).find(|&index| {
        family.len() + index <= pieces.len()
            && family.iter().zip(&pieces[index..]).all(|(name, piece)| piece.text == *name)
    })?;
    let after = version(pieces[start + family.len()..].iter().take_while(|piece| digits(piece)));
    let before: Vec<&Piece> = pieces[..start].iter().rev().take_while(|piece| digits(piece)).collect();
    Some(after.or_else(|| version(before.into_iter().rev())).map_or(Found::Bare, Found::Version))
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn model_tier(model: &str) -> ModelTier {
    let pieces = pieces(model);
    if pieces.iter().any(|piece| SMALL_MARKERS.contains(&piece.text.as_str())) {
        return ModelTier::Small;
    }
    if billions(&pieces).is_some_and(|size| size < 100) {
        return ModelTier::Small;
    }
    for (family, frontier) in FAMILIES {
        let Some(found) = family_version(&pieces, family) else { continue };
        return match found {
            Found::Version(found) if found >= frontier => ModelTier::Frontier,
            Found::Version(_) => ModelTier::Small,
            Found::Bare if family.len() == 1 && ALIASES.contains(&family[0]) => ModelTier::Frontier,
            Found::Bare => ModelTier::Unknown,
        };
    }
    ModelTier::Unknown
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn default_access(follows_model: bool, fixed: AccessMode, model: Option<String>) -> AccessMode {
    if !follows_model {
        return fixed;
    }
    model.map_or(AccessMode::Supervised, |model| model_tier(&model).access())
}
