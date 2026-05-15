//! Shared domain types for the FustAPI gateway.

use std::fmt;
use std::str::FromStr;

use crate::provider::ToolCallingSupport;

/// Supported LLM provider types.
///
/// Each variant maps to a concrete [`Provider`](crate::provider::Provider) implementation.
/// The `FromStr` impl accepts the wire format stored in the database (e.g. `"z.ai"` → `Zai`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderType {
    Omlx,
    LmStudio,
    SgLang,
    OpenAI,
    OpenAICompatible,
    DeepSeek,
    Glm,
    Zai,
}

impl ProviderType {
    /// All known provider types in stable order (used for CLI help text).
    pub const ALL: [ProviderType; 8] = [
        ProviderType::Omlx,
        ProviderType::LmStudio,
        ProviderType::SgLang,
        ProviderType::OpenAI,
        ProviderType::OpenAICompatible,
        ProviderType::DeepSeek,
        ProviderType::Glm,
        ProviderType::Zai,
    ];

    /// Default API endpoint for this provider type.
    pub fn default_endpoint(self) -> Option<&'static str> {
        match self {
            Self::Omlx => Some("http://localhost:8000/v1"),
            Self::LmStudio => Some("http://localhost:1234/v1"),
            Self::SgLang => Some("http://localhost:30000/v1"),
            Self::OpenAI => Some("https://api.openai.com/v1"),
            Self::OpenAICompatible => None,
            Self::DeepSeek => Some("https://api.deepseek.com"),
            Self::Glm => Some("https://open.bigmodel.cn/api/coding/paas/v4"),
            Self::Zai => Some("https://api.z.ai/api/paas/v4"),
        }
    }

    /// Tool calling support mode for this provider type.
    pub fn tool_calling_mode(self) -> ToolCallingSupport {
        match self {
            Self::Omlx | Self::LmStudio => ToolCallingSupport::Emulated,
            Self::SgLang
            | Self::OpenAI
            | Self::OpenAICompatible
            | Self::DeepSeek
            | Self::Glm
            | Self::Zai => ToolCallingSupport::Native,
        }
    }

    /// Whether this provider supports OpenAI `stream_options.include_usage`.
    pub fn stream_options(self) -> bool {
        matches!(self, Self::OpenAI)
    }

    /// Wire-format string used in the database and JSON API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Omlx => "omlx",
            Self::LmStudio => "lmstudio",
            Self::SgLang => "sglang",
            Self::OpenAI => "openai",
            Self::OpenAICompatible => "openai-compatible",
            Self::DeepSeek => "deepseek",
            Self::Glm => "glm",
            Self::Zai => "z.ai",
        }
    }
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "omlx" => Ok(Self::Omlx),
            "lmstudio" => Ok(Self::LmStudio),
            "sglang" => Ok(Self::SgLang),
            "openai" => Ok(Self::OpenAI),
            "openai-compatible" => Ok(Self::OpenAICompatible),
            "deepseek" => Ok(Self::DeepSeek),
            "glm" => Ok(Self::Glm),
            "z.ai" => Ok(Self::Zai),
            _ => Err(format!(
                "Unknown provider type '{s}'. Valid types: {}",
                Self::ALL
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_variants() {
        for variant in ProviderType::ALL {
            let s = variant.to_string();
            let parsed: ProviderType = s.parse().unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn zai_is_distinct_from_glm() {
        assert_ne!(ProviderType::Zai, ProviderType::Glm);
        assert_eq!(ProviderType::Zai.as_str(), "z.ai");
        assert_eq!(ProviderType::Glm.as_str(), "glm");
    }

    #[test]
    fn zai_and_glm_have_different_endpoints() {
        assert_ne!(
            ProviderType::Zai.default_endpoint(),
            ProviderType::Glm.default_endpoint()
        );
        assert_eq!(
            ProviderType::Zai.default_endpoint(),
            Some("https://api.z.ai/api/paas/v4")
        );
        assert_eq!(
            ProviderType::Glm.default_endpoint(),
            Some("https://open.bigmodel.cn/api/coding/paas/v4")
        );
    }

    #[test]
    fn unknown_type_error() {
        let err = "foobar".parse::<ProviderType>().unwrap_err();
        assert!(err.contains("Unknown provider type 'foobar'"));
    }

    #[test]
    fn default_endpoint_consistency() {
        assert!(ProviderType::Omlx.default_endpoint().is_some());
        assert!(ProviderType::LmStudio.default_endpoint().is_some());
        assert!(ProviderType::SgLang.default_endpoint().is_some());
        assert!(ProviderType::OpenAI.default_endpoint().is_some());
        assert!(ProviderType::DeepSeek.default_endpoint().is_some());
        assert!(ProviderType::Glm.default_endpoint().is_some());
        assert!(ProviderType::Zai.default_endpoint().is_some());
        // OpenAI-compatible does not (user must provide one)
        assert!(ProviderType::OpenAICompatible.default_endpoint().is_none());
    }

    #[test]
    fn tool_calling_modes() {
        use crate::provider::ToolCallingSupport;
        assert_eq!(ProviderType::Omlx.tool_calling_mode(), ToolCallingSupport::Emulated);
        assert_eq!(ProviderType::LmStudio.tool_calling_mode(), ToolCallingSupport::Emulated);
        assert_eq!(ProviderType::SgLang.tool_calling_mode(), ToolCallingSupport::Native);
        assert_eq!(ProviderType::OpenAI.tool_calling_mode(), ToolCallingSupport::Native);
        assert_eq!(ProviderType::DeepSeek.tool_calling_mode(), ToolCallingSupport::Native);
        assert_eq!(ProviderType::Glm.tool_calling_mode(), ToolCallingSupport::Native);
        assert_eq!(ProviderType::Zai.tool_calling_mode(), ToolCallingSupport::Native);
    }

    #[test]
    fn stream_options_only_openai() {
        assert!(ProviderType::OpenAI.stream_options());
        for pt in ProviderType::ALL {
            if pt != ProviderType::OpenAI {
                assert!(!pt.stream_options(), "{pt:?} should not have stream_options");
            }
        }
    }
}
