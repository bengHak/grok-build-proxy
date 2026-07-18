use serde::Serialize;

pub mod kimi;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Kimi,
}

impl Provider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Kimi => "kimi",
        }
    }

    pub const fn owned_by(self) -> &'static str {
        match self {
            Self::Codex => "openai-codex",
            Self::Kimi => "kimi",
        }
    }
}
