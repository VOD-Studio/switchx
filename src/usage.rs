//! Token usage reported by Responses upstreams and Codex session logs.
//! Only counts, model IDs, and opaque response IDs are kept; never prompts or output.

use serde_json::Value;

pub mod claude_logs;
pub mod codex_logs;
pub mod report;

/// Counts for one model response. `input` includes `cached_input`, and
/// `output` includes `reasoning`, following the Responses usage object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    pub input: u64,
    pub cached_input: u64,
    pub cache_write: u64,
    /// The one-hour subset of cache writes; the remainder uses the five-minute rate.
    pub cache_write_1h: u64,
    pub output: u64,
    pub reasoning: u64,
}

impl TokenUsage {
    /// Anthropic reports fresh input separately from cache reads and writes.
    pub fn from_claude(usage: &Value) -> Option<Self> {
        const LIMIT: u64 = i64::MAX as u64 / 16;
        let count = |path: &str| {
            usage
                .pointer(path)
                .and_then(Value::as_u64)
                .map(|n| n.min(LIMIT))
        };
        let fresh = count("/input_tokens");
        let output = count("/output_tokens");
        let read = count("/cache_read_input_tokens");
        let hour = count("/cache_creation/ephemeral_1h_input_tokens").unwrap_or(0);
        let short = count("/cache_creation/ephemeral_5m_input_tokens").unwrap_or(0);
        let writes = count("/cache_creation_input_tokens");
        if fresh.is_none()
            && output.is_none()
            && read.is_none()
            && writes.is_none()
            && hour + short == 0
        {
            return None;
        }
        let writes = writes.unwrap_or(hour + short).max(hour + short);
        Some(Self {
            input: fresh.unwrap_or(0) + read.unwrap_or(0) + writes,
            cached_input: read.unwrap_or(0),
            cache_write: writes,
            cache_write_1h: hour,
            output: output.unwrap_or(0),
            reasoning: 0,
        })
    }
    /// Reads a Responses `usage` object, or the chat-style names some compatible
    /// upstreams return. Missing input and output counts mean no usage was reported.
    pub fn from_responses(usage: &Value) -> Option<Self> {
        let count = |paths: &[&str]| paths.iter().find_map(|path| usage.pointer(path)?.as_u64());
        let input = count(&["/input_tokens", "/prompt_tokens"]);
        let output = count(&["/output_tokens", "/completion_tokens"]);
        if input.is_none() && output.is_none() {
            return None;
        }
        Some(Self::bounded(
            input.unwrap_or(0),
            count(&[
                "/input_tokens_details/cached_tokens",
                "/prompt_tokens_details/cached_tokens",
                "/prompt_cache_hit_tokens",
            ])
            .unwrap_or(0),
            count(&["/cache_write_input_tokens"]).unwrap_or(0),
            output.unwrap_or(0),
            count(&[
                "/output_tokens_details/reasoning_tokens",
                "/completion_tokens_details/reasoning_tokens",
            ])
            .unwrap_or(0),
        ))
    }

    /// Reads the usage objects Codex writes to its session logs.
    pub fn from_codex(usage: &Value) -> Option<Self> {
        let count = |name: &str| usage.get(name).and_then(Value::as_u64);
        Some(Self::bounded(
            count("input_tokens")?,
            count("cached_input_tokens").unwrap_or(0),
            count("cache_write_input_tokens").unwrap_or(0),
            count("output_tokens")?,
            count("reasoning_output_tokens").unwrap_or(0),
        ))
    }

    // Counts stay within SQLite integers, and subsets never exceed their totals.
    fn bounded(input: u64, cached: u64, cache_write: u64, output: u64, reasoning: u64) -> Self {
        let input = input.min(i64::MAX as u64 / 4);
        let output = output.min(i64::MAX as u64 / 4);
        Self {
            input,
            cached_input: cached.min(input),
            cache_write: cache_write.min(input),
            cache_write_1h: 0,
            output,
            reasoning: reasoning.min(output),
        }
    }

    pub fn total(self) -> u64 {
        self.input + self.output
    }

    /// Input billed at the full rate: neither read from nor written to the cache.
    pub fn fresh_input(self) -> u64 {
        self.input
            .saturating_sub(self.cached_input)
            .saturating_sub(self.cache_write)
    }

    pub fn is_empty(self) -> bool {
        self.total() == 0
    }

    /// Counts from a later cumulative snapshot that were not in `earlier`.
    pub fn since(self, earlier: Self) -> Self {
        Self {
            input: self.input.saturating_sub(earlier.input),
            cached_input: self.cached_input.saturating_sub(earlier.cached_input),
            cache_write: self.cache_write.saturating_sub(earlier.cache_write),
            cache_write_1h: self.cache_write_1h.saturating_sub(earlier.cache_write_1h),
            output: self.output.saturating_sub(earlier.output),
            reasoning: self.reasoning.saturating_sub(earlier.reasoning),
        }
    }
}

/// Upstream response IDs are opaque; keep only short printable ASCII ones.
pub fn response_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 256 && id.bytes().all(|b| b.is_ascii_graphic()))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_cache_components_are_disjoint_and_hour_writes_are_a_subset() {
        let usage = TokenUsage::from_claude(&json!({
            "input_tokens": 300000, "cache_read_input_tokens": 400000,
            "cache_creation_input_tokens": 300000, "output_tokens": 50000,
            "cache_creation": {"ephemeral_5m_input_tokens": 200000, "ephemeral_1h_input_tokens": 100000}
        })).unwrap();
        assert_eq!(usage.input, 1_000_000);
        assert_eq!(usage.fresh_input(), 300_000);
        assert_eq!(usage.total(), 1_050_000);
        let nested_only = TokenUsage::from_claude(&json!({"cache_creation":{"ephemeral_5m_input_tokens":2,"ephemeral_1h_input_tokens":3}})).unwrap();
        assert_eq!(
            (
                nested_only.input,
                nested_only.cache_write,
                nested_only.cache_write_1h
            ),
            (5, 5, 3)
        );
        assert!(TokenUsage::from_claude(&json!({"input_tokens":-1})).is_none());
        let bounded = TokenUsage::from_claude(&json!({"input_tokens":u64::MAX, "output_tokens":u64::MAX, "cache_read_input_tokens":u64::MAX, "cache_creation":{"ephemeral_5m_input_tokens":u64::MAX,"ephemeral_1h_input_tokens":u64::MAX}})).unwrap();
        assert!(bounded.total() < i64::MAX as u64);
        assert!(bounded.cache_write_1h <= bounded.cache_write);
    }

    #[test]
    fn responses_usage_reads_standard_and_compatible_shapes() {
        let usage = json!({
            "input_tokens": 1_000,
            "input_tokens_details": {"cached_tokens": 600},
            "output_tokens": 300,
            "output_tokens_details": {"reasoning_tokens": 120},
            "total_tokens": 1_300
        });
        let parsed = TokenUsage::from_responses(&usage).unwrap();
        assert_eq!(
            parsed,
            TokenUsage {
                cache_write_1h: 0,
                input: 1_000,
                cached_input: 600,
                cache_write: 0,
                output: 300,
                reasoning: 120
            }
        );
        assert_eq!((parsed.total(), parsed.fresh_input()), (1_300, 400));

        let chat =
            json!({"prompt_tokens": 50, "prompt_cache_hit_tokens": 20, "completion_tokens": 7});
        let parsed = TokenUsage::from_responses(&chat).unwrap();
        assert_eq!(
            (parsed.input, parsed.cached_input, parsed.output),
            (50, 20, 7)
        );

        assert!(TokenUsage::from_responses(&json!({"total_tokens": 9})).is_none());
        assert!(TokenUsage::from_responses(&json!(null)).is_none());
        assert!(TokenUsage::from_responses(&json!({"input_tokens": -1})).is_none());
    }

    #[test]
    fn subsets_are_clamped_to_their_totals() {
        let usage = json!({
            "input_tokens": 10,
            "input_tokens_details": {"cached_tokens": 99},
            "output_tokens": 3,
            "output_tokens_details": {"reasoning_tokens": 8}
        });
        let parsed = TokenUsage::from_responses(&usage).unwrap();
        assert_eq!((parsed.cached_input, parsed.reasoning), (10, 3));
        assert_eq!(parsed.fresh_input(), 0);
    }

    #[test]
    fn codex_usage_and_cumulative_differences() {
        let first = TokenUsage::from_codex(&json!({
            "input_tokens": 38_635, "cached_input_tokens": 23_296, "cache_write_input_tokens": 0,
            "output_tokens": 183, "reasoning_output_tokens": 0, "total_tokens": 38_818
        }))
        .unwrap();
        let later = TokenUsage::from_codex(&json!({
            "input_tokens": 78_372, "cached_input_tokens": 61_824,
            "output_tokens": 356, "reasoning_output_tokens": 40
        }))
        .unwrap();
        assert_eq!(
            later.since(first),
            TokenUsage {
                cache_write_1h: 0,
                input: 39_737,
                cached_input: 38_528,
                cache_write: 0,
                output: 173,
                reasoning: 40
            }
        );
        assert!(first.since(later).is_empty());
        assert!(TokenUsage::from_codex(&json!({"input_tokens": 1})).is_none());
    }

    #[test]
    fn response_ids_must_be_short_printable_ascii() {
        assert_eq!(
            response_id(&json!("resp_abc123")).as_deref(),
            Some("resp_abc123")
        );
        for value in [
            json!(""),
            json!("has space"),
            json!("中文"),
            json!(1),
            json!("x".repeat(257)),
        ] {
            assert!(response_id(&value).is_none(), "{value}");
        }
    }
}
