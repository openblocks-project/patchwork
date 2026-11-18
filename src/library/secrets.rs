//! P4.3.2B: detect obvious secrets before a graph leaves this machine.
//!
//! Node data can carry credentials: the AI Request node stores an API key
//! in the node itself, HTTP Request headers can hold `Authorization`
//! values, and dynamic node state can contain tokens. A published package
//! would share them with everyone. Publishing to the Library and
//! installing from the marketplace refuse such content; existing projects
//! are never modified (the user removes the value).
//!
//! Detection is heuristic: credential-named fields with a value, credential
//! headers, and well-known key formats. Reports never include the secret.

use serde_json::Value;

/// One suspected secret. Never contains the secret itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    /// Where it is (`graph.nodes.4.node_type.AiRequest.api_key`).
    pub location: String,
    pub kind: &'static str,
}

impl std::fmt::Display for SecretFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.location, self.kind)
    }
}

/// Field names whose non-empty value is a credential (compared without
/// case, `_` or `-`). Names of a stored key (`api_key_name`) are not.
const SECRET_FIELDS: &[&str] = &[
    "apikey",
    "apitoken",
    "accesstoken",
    "authtoken",
    "bearertoken",
    "refreshtoken",
    "token",
    "secret",
    "secretkey",
    "clientsecret",
    "accesskey",
    "privatekey",
    "password",
    "passwd",
    "authorization",
];

/// Header names whose value is a credential.
const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-auth-token",
    "cookie",
];

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// A well-known credential format.
fn key_format(word: &str) -> Option<&'static str> {
    let word = word.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';'));
    let tail_ok = |prefix: &str, min: usize| {
        word.len() >= min
            && word[prefix.len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    };
    if word.starts_with("sk-ant-") && tail_ok("sk-ant-", 30) {
        return Some("Anthropic API key");
    }
    if word.starts_with("sk-") && tail_ok("sk-", 30) {
        return Some("API key (sk-…)");
    }
    if word.starts_with("AIza") && word.len() == 39 && tail_ok("AIza", 39) {
        return Some("Google API key");
    }
    for prefix in ["ghp_", "gho_", "ghs_", "github_pat_"] {
        if word.starts_with(prefix) && tail_ok(prefix, 30) {
            return Some("GitHub token");
        }
    }
    for prefix in ["xoxb-", "xoxp-", "xoxa-"] {
        if word.starts_with(prefix) && tail_ok(prefix, 20) {
            return Some("Slack token");
        }
    }
    if word.len() == 20
        && word.starts_with("AKIA")
        && word
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Some("AWS access key");
    }
    None
}

/// Credentials inside a free-text value (header lines, key formats).
fn text_findings(text: &str) -> Option<&'static str> {
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return Some("private key");
    }
    for line in text.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if SECRET_HEADERS.contains(&name.trim().to_ascii_lowercase().as_str())
                && !value.trim().is_empty()
            {
                return Some("credential header");
            }
        }
    }
    text.split_whitespace().find_map(key_format)
}

fn walk(value: &Value, location: &mut Vec<String>, out: &mut Vec<SecretFinding>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                location.push(key.clone());
                let filled = child.as_str().is_some_and(|s| !s.trim().is_empty());
                if filled && SECRET_FIELDS.contains(&normalized(key).as_str()) {
                    out.push(SecretFinding {
                        location: location.join("."),
                        kind: "credential field",
                    });
                } else {
                    walk(child, location, out);
                }
                location.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                location.push(i.to_string());
                walk(child, location, out);
                location.pop();
            }
        }
        Value::String(text) => {
            if let Some(kind) = text_findings(text) {
                out.push(SecretFinding {
                    location: location.join("."),
                    kind,
                });
            }
        }
        _ => {}
    }
}

/// Suspected secrets in a JSON document.
pub fn find_secrets(value: &Value) -> Vec<SecretFinding> {
    let mut out = Vec::new();
    walk(value, &mut Vec::new(), &mut out);
    out
}

/// Suspected secrets in a JSON text (`.pwnode`, `package.json`); text that
/// is not JSON is checked as plain text.
pub fn find_secrets_in_text(text: &str) -> Vec<SecretFinding> {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => find_secrets(&value),
        Err(_) => text_findings(text)
            .map(|kind| {
                vec![SecretFinding {
                    location: String::new(),
                    kind,
                }]
            })
            .unwrap_or_default(),
    }
}

/// Refuse a folder (a staged package version) holding a suspected secret
/// in any of its files.
pub fn check_folder(dir: &std::path::Path) -> Result<(), String> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        let entries = std::fs::read_dir(&folder).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let findings = find_secrets_in_text(&text);
            if !findings.is_empty() {
                let name = path
                    .strip_prefix(dir)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                return Err(describe(&name, &findings));
            }
        }
    }
    Ok(())
}

/// A refusal message for `file`, listing at most five findings.
pub fn describe(file: &str, findings: &[SecretFinding]) -> String {
    let listed: Vec<String> = findings.iter().take(5).map(|f| f.to_string()).collect();
    let more = findings.len().saturating_sub(5);
    format!(
        "{file} contains what looks like a secret: {}{}. Remove it (for example, use a stored \
         key name instead of the key) and try again; it is not removed automatically.",
        listed.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credential_fields_headers_and_key_formats_are_found() {
        let prompt = format!("use sk-proj-{} please", "a".repeat(40));
        let graph = json!({"graph": {"nodes": {
            "1": {"node_type": {"AiRequest": {"api_key": "abc123", "api_key_name": "work"}}},
            "2": {"node_type": {"HttpRequest": {"headers": "Accept: json\nAuthorization: Bearer xyz"}}},
            "3": {"node_type": {"Dynamic": {"inner": {"type_tag": "text_gen", "state": {
                "prompt": prompt
            }}}}},
            "4": {"node_type": {"Dynamic": {"inner": {"state": {"accessToken": "t0k3n"}}}}}
        }}});
        let found = find_secrets(&graph);
        let locations: Vec<&str> = found.iter().map(|f| f.location.as_str()).collect();
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(locations.contains(&"graph.nodes.1.node_type.AiRequest.api_key"));
        assert!(
            found.iter().all(|f| !f.to_string().contains("abc123")),
            "never echoed"
        );
        assert!(found.iter().any(|f| f.kind == "credential header"));
        assert!(found.iter().any(|f| f.kind == "API key (sk-…)"));
    }

    #[test]
    fn ordinary_content_is_not_flagged() {
        let graph = json!({"graph": {"nodes": {
            "1": {"node_type": {"AiRequest": {"api_key": "", "api_key_name": "work"}}},
            "2": {"node_type": {"HttpRequest": {"headers": "Accept: application/json\nAuthorization:"}}},
            "3": {"node_type": {"Slider": {"label": "token count", "value": 0.5}}},
            "4": {"node_type": {"Dynamic": {"inner": {"state": {"text": "sk-short", "token_limit": 100}}}}}
        }}});
        assert!(
            find_secrets(&graph).is_empty(),
            "{:?}",
            find_secrets(&graph)
        );
    }

    #[test]
    fn known_formats() {
        for (word, kind) in [
            (format!("sk-ant-{}", "a".repeat(40)), "Anthropic API key"),
            (format!("AIza{}", "b".repeat(35)), "Google API key"),
            (format!("ghp_{}", "c".repeat(36)), "GitHub token"),
            ("AKIAABCDEFGHIJKLMNOP".to_string(), "AWS access key"),
        ] {
            assert_eq!(key_format(&word), Some(kind), "{word}");
        }
        assert!(text_findings("-----BEGIN RSA PRIVATE KEY-----\nMII").is_some());
        assert!(
            describe("Gain.pwnode", &find_secrets_in_text(r#"{"password":"x"}"#))
                .contains("password")
        );
    }
}
