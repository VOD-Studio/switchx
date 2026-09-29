//! Offline provider icon catalog from CC Switch's bundled picker.
//! Only catalog IDs are accepted; no user paths, URLs, or SVG are loaded.

use crate::storage::ProviderKind;

pub struct ProviderIcon {
    pub id: &'static str,
    pub name: &'static str,
    pub keywords: &'static [&'static str],
    /// Standalone SVG, including embedded data for the original raster icons.
    pub data: &'static [u8],
    pub monochrome: bool,
    /// Brand color already baked into the SVG; neutral icons follow the theme.
    pub color: Option<&'static str>,
}

macro_rules! provider_icon {
    ($id:literal, $name:literal, [$($keyword:literal),*], $monochrome:literal, $color:expr) => {
        ProviderIcon {
            id: $id,
            name: $name,
            keywords: &[$($keyword),*],
            data: include_bytes!(concat!("../assets/providers/icons/", $id, ".svg")),
            monochrome: $monochrome,
            color: $color,
        }
    };
}

// CC Switch 846de29: index.ts whitelist with metadata.ts labels and keywords.
// Keep this list sorted so picker order remains stable.
// The four existing SwitchX presets retain their original artwork and tint rules.
pub const PROVIDER_ICONS: &[ProviderIcon] = &[
    provider_icon!(
        "9527code",
        "9527CODE",
        [
            "9527code",
            "9527",
            "codes",
            "aggregator",
            "relay",
            "gateway"
        ],
        true,
        None
    ),
    provider_icon!(
        "a6api",
        "A6API",
        ["a6api", "a6", "aggregator", "relay", "gateway", "claude"],
        false,
        Some("#3B82F6")
    ),
    provider_icon!("aicodemirror", "aicodemirror", [], false, None),
    provider_icon!(
        "aicodewith",
        "AICodeWith",
        [
            "aicodewith",
            "ai code with",
            "aggregator",
            "relay",
            "gateway",
            "claude",
            "codex",
            "gemini"
        ],
        true,
        None
    ),
    provider_icon!("aicoding", "aicoding", [], false, None),
    provider_icon!(
        "aigocode",
        "AIGoCode",
        ["aigocode", "aigo", "code", "third-party"],
        false,
        Some("#5B7FFF")
    ),
    provider_icon!(
        "aihubmix",
        "AiHubMix",
        ["aihubmix", "hub", "mix", "aggregator"],
        false,
        Some("#006FFB")
    ),
    provider_icon!(
        "alibaba",
        "Alibaba",
        ["qwen", "tongyi"],
        false,
        Some("#FF6A00")
    ),
    provider_icon!(
        "amux",
        "Amux",
        ["amux", "amuxapi", "aggregator", "relay", "gateway", "gpt"],
        true,
        None
    ),
    provider_icon!("anthropic", "Anthropic", ["claude"], false, Some("#D4915D")),
    provider_icon!(
        "apikeyfun",
        "APIKEY.FUN",
        [
            "apikeyfun",
            "api key",
            "gateway",
            "relay",
            "claude",
            "codex",
            "gemini"
        ],
        false,
        Some("#9C3F00")
    ),
    provider_icon!(
        "apinebula",
        "APINebula",
        [
            "apinebula",
            "api nebula",
            "gateway",
            "relay",
            "claude",
            "codex",
            "gemini"
        ],
        false,
        Some("#C86F49")
    ),
    provider_icon!(
        "atlascloud",
        "AtlasCloud",
        [
            "atlascloud",
            "atlas cloud",
            "coding plan",
            "openai",
            "anthropic",
            "codex",
            "claude"
        ],
        false,
        Some("#111111")
    ),
    provider_icon!("aws", "AWS", ["amazon", "cloud"], false, Some("#FF9900")),
    provider_icon!(
        "azure",
        "Azure",
        ["microsoft", "cloud"],
        false,
        Some("#0078D4")
    ),
    provider_icon!(
        "baidu",
        "Baidu",
        ["ernie", "wenxin"],
        false,
        Some("#2932E1")
    ),
    provider_icon!(
        "bailian",
        "Bailian",
        ["bailian", "dashscope", "aliyun", "alibaba"],
        false,
        Some("#624AFF")
    ),
    provider_icon!("bytedance", "bytedance", [], false, None),
    provider_icon!(
        "byteplus",
        "BytePlus",
        ["byteplus", "volcengine", "ark", "modelark"],
        false,
        Some("#3370FF")
    ),
    provider_icon!("catcoder", "catcoder", [], true, None),
    provider_icon!(
        "ccsub",
        "CCSub",
        ["ccsub", "aggregator", "relay", "claude", "codex", "gateway"],
        false,
        Some("#1E88E5")
    ),
    provider_icon!("chatglm", "chatglm", [], false, None),
    provider_icon!(
        "cherryin",
        "CherryIN",
        [
            "cherryin", "cherry", "gateway", "relay", "newapi", "claude", "codex"
        ],
        false,
        Some("#FB6354")
    ),
    provider_icon!("claude", "Claude", ["anthropic"], false, Some("#D4915D")),
    provider_icon!(
        "claudeapi",
        "ClaudeAPI",
        ["claudeapi", "claude", "anthropic", "bedrock"],
        false,
        None
    ),
    provider_icon!(
        "claudecn",
        "ClaudeCN",
        ["claudecn", "claude", "enterprise"],
        false,
        None
    ),
    provider_icon!(
        "cloudflare",
        "Cloudflare",
        ["cloudflare", "cdn"],
        false,
        Some("#F38020")
    ),
    provider_icon!(
        "code0",
        "Code0",
        ["code0", "code0ai", "aggregator", "relay", "gateway", "gpt"],
        false,
        Some("#20C050")
    ),
    provider_icon!("cohere", "Cohere", ["cohere"], false, Some("#39594D")),
    provider_icon!("copilot", "copilot", [], false, None),
    provider_icon!("crazyrouter", "crazyrouter", [], true, None),
    provider_icon!(
        "cubence",
        "Cubence",
        ["cubence", "api", "relay"],
        false,
        Some("#4B5563")
    ),
    provider_icon!(
        "deepseek",
        "DeepSeek",
        ["deep", "seek"],
        false,
        Some("#1E88E5")
    ),
    provider_icon!("doubao", "doubao", [], false, None),
    provider_icon!(
        "eflowcode",
        "E-FlowCode",
        ["eflowcode", "e-flowcode", "flow"],
        false,
        None
    ),
    provider_icon!(
        "etok",
        "ETok",
        ["etok", "ai", "programming"],
        false,
        Some("#F97316")
    ),
    provider_icon!(
        "fenno",
        "FennoAI",
        [
            "fenno",
            "fennoai",
            "aggregator",
            "relay",
            "claude",
            "codex",
            "gpt",
            "gateway"
        ],
        false,
        Some("#000000")
    ),
    provider_icon!(
        "fluxa",
        "FluxA",
        [
            "fluxa",
            "fluxapay",
            "agentmarket",
            "tokenplan",
            "marketplace"
        ],
        false,
        None
    ),
    provider_icon!("gemini", "Gemini", ["google"], false, Some("#4285F4")),
    provider_icon!("gemma", "gemma", [], false, None),
    provider_icon!("github", "GitHub", ["git", "version control"], true, None),
    provider_icon!("githubcopilot", "githubcopilot", [], true, None),
    provider_icon!(
        "google",
        "Google",
        ["gemini", "bard"],
        false,
        Some("#4285F4")
    ),
    provider_icon!("googlecloud", "googlecloud", [], false, None),
    provider_icon!("grok", "grok", [], true, None),
    provider_icon!(
        "hermes",
        "Hermes",
        ["hermes", "agent", "nous", "nousresearch"],
        false,
        Some("#000000")
    ),
    provider_icon!(
        "huawei",
        "Huawei",
        ["huawei", "cloud"],
        false,
        Some("#FF0000")
    ),
    provider_icon!(
        "huggingface",
        "Hugging Face",
        ["huggingface", "hf"],
        false,
        Some("#FFD21E")
    ),
    provider_icon!("hunyuan", "hunyuan", [], false, None),
    provider_icon!(
        "huoshan",
        "火山方舟",
        ["huoshan", "volcengine", "ark", "agentplan", "byteplus"],
        false,
        Some("#3370FF")
    ),
    provider_icon!(
        "jiekou",
        "JieKou AI",
        ["jiekou", "jiekou ai", "interface ai", "aggregator"],
        false,
        Some("#000000")
    ),
    provider_icon!("kimi", "Kimi", ["moonshot"], true, None),
    provider_icon!(
        "lioncc",
        "LionCC",
        ["lioncc", "lion"],
        false,
        Some("#F9DA3C")
    ),
    provider_icon!(
        "longcat",
        "LongCat",
        ["longcat", "long", "cat"],
        false,
        Some("#29E154")
    ),
    provider_icon!("mcp", "mcp", [], true, None),
    provider_icon!(
        "meta",
        "Meta",
        ["facebook", "llama"],
        false,
        Some("#0081FB")
    ),
    provider_icon!("micu", "micu", [], false, None),
    provider_icon!("midjourney", "midjourney", [], true, None),
    provider_icon!("minimax", "MiniMax", ["minimax"], false, Some("#FF6B6B")),
    provider_icon!("mistral", "Mistral", ["mistral"], false, Some("#FF7000")),
    provider_icon!(
        "modelscope",
        "ModelScope",
        ["modelscope", "alibaba", "scope"],
        false,
        Some("#624AFF")
    ),
    provider_icon!(
        "nekocode",
        "NekoCode",
        ["nekocode", "neko", "aggregator", "relay", "gateway", "gpt"],
        false,
        Some("#A64BC4")
    ),
    provider_icon!("newapi", "newapi", [], false, None),
    provider_icon!("notion", "notion", [], true, None),
    provider_icon!("novita", "Novita AI", ["novita", "novita ai"], true, None),
    provider_icon!(
        "nvidia",
        "NVIDIA",
        ["nvidia", "nim", "gpu"],
        false,
        Some("#74B71B")
    ),
    provider_icon!("ollama", "ollama", [], true, None),
    provider_icon!(
        "openai",
        "OpenAI",
        ["gpt", "chatgpt"],
        false,
        Some("#00A67E")
    ),
    provider_icon!(
        "openclaw",
        "OpenClaw",
        ["openclaw", "lobster", "claw"],
        false,
        Some("#ff4f40")
    ),
    provider_icon!("opencode", "opencode", [], true, None),
    provider_icon!(
        "openrouter",
        "OpenRouter",
        ["openrouter", "router", "aggregator"],
        false,
        Some("#6566F1")
    ),
    provider_icon!(
        "packycode",
        "PackyCode",
        ["packycode", "packy", "packyapi"],
        true,
        None
    ),
    provider_icon!("palm", "palm", [], false, None),
    provider_icon!(
        "pateway",
        "PatewayAI",
        ["pateway", "patewayai", "claude", "codex"],
        false,
        None
    ),
    provider_icon!(
        "perplexity",
        "Perplexity",
        ["perplexity"],
        false,
        Some("#20808D")
    ),
    provider_icon!("pi", "pi", [], true, None),
    provider_icon!("pipellm", "PIPELLM", ["pipellm", "pipe"], false, None),
    provider_icon!("ppio", "PPIO", ["ppio", "派欧云"], false, Some("#2874FF")),
    provider_icon!(
        "qianwenai",
        "千问AI平台",
        ["qianwenai", "qianwen", "qwen", "aliyun", "alibaba"],
        false,
        Some("#624AFF")
    ),
    provider_icon!(
        "qiniu",
        "七牛云",
        [
            "qiniu",
            "qnaigc",
            "modelink",
            "aggregator",
            "relay",
            "claude",
            "codex",
            "gpt",
            "gemini",
            "gateway"
        ],
        false,
        Some("#00AAE7")
    ),
    provider_icon!("qwen", "qwen", [], false, None),
    provider_icon!(
        "qwencloud",
        "QwenCloud",
        ["qwencloud", "qwen", "aliyun", "alibaba"],
        false,
        Some("#6336E7")
    ),
    provider_icon!("rc", "rc", [], false, None),
    provider_icon!(
        "relaxcode",
        "RelaxyCode",
        ["relaxycode", "relaxcode", "relax"],
        false,
        None
    ),
    provider_icon!(
        "runapi",
        "RunAPI",
        ["runapi", "run", "aggregator", "gateway"],
        false,
        None
    ),
    provider_icon!(
        "shengsuanyun",
        "Shengsuanyun",
        ["shengsuanyun", "shengsuanyun"],
        false,
        None
    ),
    provider_icon!("siliconflow", "siliconflow", [], false, None),
    provider_icon!(
        "soleapi",
        "SoleAPI",
        [
            "soleapi",
            "sole",
            "aggregator",
            "relay",
            "gateway",
            "claude"
        ],
        true,
        None
    ),
    provider_icon!(
        "soshow",
        "Soshow",
        ["soshow", "so-show", "model market", "aggregator", "claude"],
        false,
        Some("#7966FE")
    ),
    provider_icon!("sssaicode", "sssaicode", [], false, None),
    provider_icon!("stability", "stability", [], false, None),
    provider_icon!(
        "stepfun",
        "StepFun",
        ["stepfun", "step", "jieyue", "阶跃星辰"],
        false,
        Some("#005AFF")
    ),
    provider_icon!(
        "sub2api",
        "Sub2API",
        [
            "sub2api",
            "sub2",
            "aggregator",
            "relay",
            "gateway",
            "claude",
            "codex",
            "gemini"
        ],
        false,
        Some("#39D9E7")
    ),
    provider_icon!(
        "subrouter",
        "SubRouter",
        [
            "subrouter",
            "subrouter.ai",
            "aggregator",
            "relay",
            "claude",
            "codex",
            "gemini",
            "gateway"
        ],
        false,
        Some("#0D9488")
    ),
    provider_icon!(
        "sudocode",
        "SudoCode.chat",
        [
            "sudocode",
            "sudo code",
            "chat",
            "gateway",
            "relay",
            "claude",
            "codex",
            "gemini",
            "openclaw"
        ],
        false,
        Some("#111111")
    ),
    provider_icon!(
        "sudocode-us",
        "SudoCode.us",
        [
            "sudocode",
            "sudo code",
            "us",
            "gateway",
            "relay",
            "claude",
            "codex",
            "gemini",
            "openclaw"
        ],
        false,
        Some("#111111")
    ),
    provider_icon!(
        "teamorouter",
        "TeamoRouter",
        [
            "teamorouter",
            "teamo",
            "router",
            "aggregator",
            "relay",
            "gateway",
            "gpt"
        ],
        false,
        Some("#000000")
    ),
    provider_icon!("tencent", "Tencent", ["hunyuan"], false, Some("#00A4FF")),
    provider_icon!("ucloud", "ucloud", [], false, None),
    provider_icon!(
        "unity2",
        "Unity2.ai",
        [
            "unity2",
            "aggregator",
            "relay",
            "claude",
            "codex",
            "gateway"
        ],
        false,
        Some("#000000")
    ),
    provider_icon!("vercel", "vercel", [], true, None),
    provider_icon!("wenxin", "wenxin", [], false, None),
    provider_icon!("xai", "xai", [], true, None),
    provider_icon!(
        "xiaomimimo",
        "Xiaomi MiMo",
        ["xiaomimimo", "xiaomi", "mimo"],
        true,
        None
    ),
    provider_icon!(
        "xycai",
        "XycAi",
        ["xycai", "xyc", "aggregator", "relay", "gateway", "token"],
        false,
        Some("#1E88E5")
    ),
    provider_icon!("yi", "yi", [], false, None),
    provider_icon!(
        "zenmux",
        "ZenMux",
        ["zenmux", "zen", "mux"],
        false,
        Some("#6366F1")
    ),
    provider_icon!("zeroone", "zeroone", [], true, None),
    provider_icon!(
        "zetaapi",
        "ZetaAPI",
        [
            "zetaapi",
            "zeta",
            "aggregator",
            "relay",
            "claude",
            "gpt",
            "gateway"
        ],
        false,
        Some("#000000")
    ),
    provider_icon!(
        "zhipu",
        "Zhipu AI",
        ["chatglm", "glm"],
        false,
        Some("#0F62FE")
    ),
];

pub fn icon(id: &str) -> Option<&'static ProviderIcon> {
    let id = id.trim();
    PROVIDER_ICONS
        .iter()
        .find(|icon| icon.id.eq_ignore_ascii_case(id))
}

/// Keep vector artwork scalable while correcting invalid resampled alpha in embedded bitmaps.
pub fn load_image(icon: &ProviderIcon) -> Result<slint::Image, slint::LoadImageError> {
    let image = slint::Image::load_from_svg_data(icon.data)?;
    // resvg can overshoot alpha when downscaling an embedded PNG (for example A6API).
    // The software renderer requires every premultiplied color channel to be <= alpha.
    if icon.data.windows(6).any(|tag| tag == b"<image") {
        return Ok(image
            .to_rgba8()
            .map(slint::Image::from_rgba8)
            .unwrap_or(image));
    }
    Ok(image)
}

pub fn search(query: &str) -> Vec<&'static ProviderIcon> {
    let query = query.trim().to_lowercase();
    PROVIDER_ICONS
        .iter()
        .filter(|icon| {
            icon.id.contains(&query)
                || icon.name.to_lowercase().contains(&query)
                || icon
                    .keywords
                    .iter()
                    .any(|keyword| keyword.to_lowercase().contains(&query))
        })
        .collect()
}

/// Defaults use the saved provider kind and exact known endpoint, never its name.
pub fn default_icon_id(kind: ProviderKind, base_url: &str) -> &'static str {
    if kind == ProviderKind::Chatgpt {
        "openai"
    } else {
        crate::app::provider_preset(base_url).map_or("", |preset| preset.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_a_sorted_unique_embedded_whitelist() {
        assert_eq!(PROVIDER_ICONS.len(), 110);
        assert!(
            PROVIDER_ICONS
                .windows(2)
                .all(|icons| icons[0].id < icons[1].id)
        );
        for entry in PROVIDER_ICONS {
            assert!(
                entry
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            );
            assert!(!entry.data.is_empty());
            assert_eq!(icon(entry.id).unwrap().id, entry.id);
        }
        assert!(icon("https://example.invalid/icon.svg").is_none());
        assert!(icon("../../private.svg").is_none());
        assert!(icon("missing").is_none());
        assert_eq!(icon(" OPENAI ").unwrap().id, "openai");
    }

    #[test]
    fn search_matches_labels_and_keywords_in_any_case() {
        assert_eq!(search("").len(), PROVIDER_ICONS.len());
        assert!(search("CHATGPT").iter().any(|icon| icon.id == "openai"));
        assert!(search("阶跃星辰").iter().any(|icon| icon.id == "stepfun"));
        assert!(
            search(" Hugging Face ")
                .iter()
                .any(|icon| icon.id == "huggingface")
        );
        assert!(search("no-such-provider-icon").is_empty());
    }

    #[test]
    fn defaults_use_provider_kind_and_exact_known_endpoint() {
        assert_eq!(default_icon_id(ProviderKind::Chatgpt, ""), "openai");
        assert_eq!(
            default_icon_id(ProviderKind::ApiKey, "https://api.deepseek.com/v1/"),
            "deepseek"
        );
        assert_eq!(
            default_icon_id(ProviderKind::ApiKey, "https://api.moonshot.cn/v1"),
            "kimi"
        );
        assert_eq!(
            default_icon_id(ProviderKind::ApiKey, "https://api.minimax.cn/v1"),
            "minimax"
        );
        assert_eq!(
            default_icon_id(ProviderKind::ApiKey, "https://api.xiaomimimo.com/v1"),
            "xiaomimimo"
        );
        assert_eq!(
            default_icon_id(
                ProviderKind::ApiKey,
                "https://api.deepseek.com.example.invalid/v1"
            ),
            ""
        );
        assert_eq!(
            default_icon_id(ProviderKind::ApiKey, "https://example.invalid/deepseek"),
            ""
        );
    }
}
