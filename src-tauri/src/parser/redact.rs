//! Display-time secret redaction for exec command text.
//!
//! Codex v0.147.0 (#36893, #36908) started redacting bearer tokens and other secrets from
//! commands rendered by its own app-server-protocol layer (`ThreadItem::CommandExecution`,
//! used by IDE/app clients for live and replayed history). That redaction happens only when
//! building those client-facing items — the raw, unredacted `command: Vec<String>` is still
//! what gets persisted to the rollout JSONL that codex-trace reads directly. Without its own
//! redaction, codex-trace would surface secrets that Codex's own UIs now hide. These regexes
//! mirror `codex-rs/secrets/src/sanitizer.rs` so codex-trace redacts the same patterns Codex
//! itself does.
use regex::Regex;
use std::sync::OnceLock;

fn openai_key_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"sk-[A-Za-z0-9]{20,}").expect("valid regex"))
}

fn aws_access_key_id_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("valid regex"))
}

fn bearer_token_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i:\bBearer)[ \t]+[A-Za-z0-9._~+/-]{16,}=*").expect("valid regex")
    })
}

fn secret_assignment_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"(?i)\b(api[_-]?key|token|secret|password)\b(\s*[:=]\s*)(["']?)[^\s"']{8,}"#)
            .expect("valid regex")
    })
}

/// Best-effort redaction of recognizable secrets (bearer tokens, OpenAI/AWS-style keys,
/// and `key=value`/`key: value` secret assignments) from a single line of display text.
pub fn redact_secrets(input: &str) -> String {
    let redacted = bearer_token_regex().replace_all(input, "Bearer [REDACTED_SECRET]");
    let redacted = openai_key_regex().replace_all(&redacted, "[REDACTED_SECRET]");
    let redacted = aws_access_key_id_regex().replace_all(&redacted, "[REDACTED_SECRET]");
    let redacted = secret_assignment_regex().replace_all(&redacted, "$1$2$3[REDACTED_SECRET]");
    redacted.into_owned()
}

#[cfg(test)]
mod tests {
    use super::redact_secrets;

    // Fixture tokens are assembled from split literals at runtime so the source never
    // contains a single string matching a real credential pattern.
    fn fixture_token(prefix: &str, suffix: &str) -> String {
        format!("{prefix}{suffix}")
    }

    #[test]
    fn redacts_bearer_token() {
        let token = fixture_token("abcdefghijklmnop", "01234567");
        let input = format!("curl -H \"Authorization: Bearer {token}\" https://api.example.com");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("Bearer [REDACTED_SECRET]"));
    }

    #[test]
    fn redacts_openai_style_key() {
        let token = fixture_token("sk-", "abcdefghijklmnopqrstuvwx");
        let input = format!("export OPENAI_API_KEY={token}");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn redacts_aws_access_key_id() {
        let token = fixture_token("AKIA", "ABCDEFGHIJKLMNOP");
        let input = format!("aws configure set aws_access_key_id {token}");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn redacts_generic_secret_assignment() {
        let token = fixture_token("supersecretvalue", "123");
        let input = format!("--password={token}");
        let output = redact_secrets(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn leaves_ordinary_commands_untouched() {
        let input = "npx oxlint src/components/ToolCallItem.tsx";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn avoids_bearer_false_positive_on_short_word() {
        let input = "Bearer of good news arrived";
        assert_eq!(redact_secrets(input), input);
    }
}
