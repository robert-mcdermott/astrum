//! Keeping secrets away from the model. What the agent collects for it
//! (command output, file contents, the terminal's text) is searched for
//! keys, tokens and passwords, and each is replaced by a note saying what
//! was there. The user's terminal still shows the real thing.

use regex_automata::meta::Regex;
use regex_automata::util::captures::Captures;
use std::sync::OnceLock;

/// What to look for: what to call it, the expression, and which group of
/// the expression is the secret itself (0: all of it).
const PATTERNS: &[(&str, &str, usize)] = &[
    ("private key", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----", 0),
    ("AWS access key", r"(?-u:\b)(?:AKIA|ASIA)[0-9A-Z]{16}(?-u:\b)", 0),
    ("GitHub token", r"(?-u:\b)(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{50,})", 0),
    ("API key", r"(?-u:\b)sk-(?:ant-|proj-|or-)?[A-Za-z0-9_-]{20,}", 0),
    ("Slack token", r"(?-u:\b)xox[abprs]-[A-Za-z0-9-]{10,}", 0),
    ("Google API key", r"(?-u:\b)AIza[0-9A-Za-z_-]{35}(?-u:\b)", 0),
    ("payment key", r"(?-u:\b)[rs]k_(?:live|test)_[0-9a-zA-Z]{16,}", 0),
    ("token", r"(?-u:\b)eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}", 0),
    ("credential", r"(?i)(?-u:\b)(?:proxy-)?authorization\s*[:=]\s*(?:bearer|basic|token)\s+([A-Za-z0-9._~+/=-]{8,})", 1),
    ("password", r"[a-zA-Z][a-zA-Z0-9+.-]*://[^/\s:@]+:([^@\s/]{3,})@", 1),
    // A whole line that sets something whose name says it is secret, as in
    // an `env` listing or a .env file, possibly behind a line number
    // (read_file) or `path:line:` (search_files). A value that is a
    // variable or a command substitution is left alone, and so is anything
    // with more after it on the line, which is how code looks.
    (
        "secret",
        r#"(?m)^[ \t]*(?:\d+[ \t]+|[^\s:]+:\d+:[ \t]*)?(?:export[ \t]+)?[A-Za-z0-9_]*(?i:SECRET|TOKEN|PASSWORD|PASSWD|API_?KEY|ACCESS_KEY|PRIVATE_KEY|CREDENTIALS?)[A-Za-z0-9_]*=["']?([^\s$"'`(){}\[\]<>,;][^\s"'`(),;]{5,})["']?[ \t]*$"#,
        1,
    ),
    // The same given as a command-line option: --password=..., --api-key=...
    ("secret", r#"--(?i:[a-z-]*(?:password|passwd|token|secret|api-?key)[a-z-]*)=([^\s"'<>\[\]$]{6,})"#, 1),
    // `name: value` or `name = "value"` where the name says secret and the
    // value looks like a generated one: long, with letters and digits.
    // Ordinary code (`token = get_token()`, `password: str`) does not match.
    (
        "secret",
        r#"(?i)(?:secret|token|passw(?:or)?d|api[_-]?key|access[_-]?key|private[_-]?key|credential)[a-z0-9_-]*["']?\s*[:=]\s*["']?([A-Za-z0-9+/_-]{20,}={0,2})"#,
        1,
    ),
];

fn compiled() -> &'static [(&'static str, Regex, usize)] {
    static CELL: OnceLock<Vec<(&'static str, Regex, usize)>> = OnceLock::new();
    CELL.get_or_init(|| {
        PATTERNS.iter().filter_map(|(kind, pattern, group)| Regex::new(pattern).ok().map(|re| (*kind, re, *group))).collect()
    })
}

/// A long value that is all letters, or all digits, is a word or a number
/// rather than a generated secret.
fn looks_generated(value: &str) -> bool {
    value.chars().any(|c| c.is_ascii_digit()) && value.chars().any(|c| c.is_ascii_alphabetic())
}

/// `text` with every secret found in it replaced, and how many there were.
/// `known` are secrets Astrum holds itself (its providers' keys), which are
/// removed wherever they appear.
pub fn scrub(text: &str, known: &[String]) -> (String, usize) {
    let mut out = text.to_string();
    let mut count = 0;
    for secret in known.iter().filter(|s| s.len() >= 8) {
        let found = out.matches(secret.as_str()).count();
        if found > 0 {
            out = out.replace(secret.as_str(), "[redacted: one of your API keys]");
            count += found;
        }
    }
    for (index, (kind, re, group)) in compiled().iter().enumerate() {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut caps: Captures = re.create_captures();
        let mut at = 0;
        while at <= out.len() {
            re.search_captures(&regex_automata::Input::new(&out).range(at..), &mut caps);
            let (Some(whole), Some(span)) = (caps.get_match(), caps.get_group(*group)) else { break };
            at = whole.end().max(whole.start() + 1);
            let value = &out[span.start..span.end];
            // Something already hidden, or (for the last, loosest pattern)
            // a plain word, is left as it is.
            let generic = index == PATTERNS.len() - 1;
            if value.starts_with("[redacted") || value.contains("redacted:") || (generic && !looks_generated(value)) {
                continue;
            }
            spans.push((span.start, span.end));
        }
        count += spans.len();
        for (start, end) in spans.into_iter().rev() {
            out.replace_range(start..end, &format!("[redacted: {kind}]"));
        }
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made-up values in the shape of real ones.
    const GITHUB: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    const AWS_ID: &str = "AKIAIOSFODNN7EXAMPLE";

    fn hidden(text: &str) -> (String, usize) {
        scrub(text, &[])
    }

    #[test]
    fn every_pattern_compiles() {
        assert_eq!(compiled().len(), PATTERNS.len());
    }

    #[test]
    fn well_known_key_shapes_are_removed() {
        for (text, kind) in [
            (format!("token is {GITHUB} ok"), "GitHub token"),
            (format!("aws_access_key_id = {AWS_ID}"), "AWS access key"),
            ("key sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345 end".to_string(), "API key"),
            ("OPENAI: sk-proj-abcdefghijklmnopqrstuvwxyz0123".to_string(), "API key"),
            ("slack xoxb-123456789012-abcdefghijkl".to_string(), "Slack token"),
            ("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnop".to_string(), "token"),
            ("curl -H 'Authorization: Bearer abc123def456ghi789' https://x".to_string(), "credential"),
            ("git remote: https://alice:hunter2secret@github.com/a/b.git".to_string(), "password"),
        ] {
            let (out, n) = hidden(&text);
            assert!(n >= 1 && out.contains(&format!("[redacted: {kind}]")), "{text} -> {out}");
        }
        let (out, n) = hidden(&format!("first {GITHUB}\nsecond {GITHUB}\n"));
        assert_eq!(n, 2);
        assert!(!out.contains("ghp_") && out.starts_with("first [redacted: GitHub token]\nsecond "));
    }

    #[test]
    fn private_keys_go_whole() {
        let pem = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAAB3NzaC1yc2EAAAADAQAB\n-----END OPENSSH PRIVATE KEY-----\nafter\n";
        let (out, n) = hidden(pem);
        assert_eq!(n, 1);
        assert_eq!(out, "before\n[redacted: private key]\nafter\n");
    }

    #[test]
    fn environment_listings_lose_their_secret_values_only() {
        let env = "HOME=/Users/sam\nexport DB_PASSWORD=correct-horse-battery\nAPI_KEY='abc123'\nGITHUB_TOKEN=$(gh auth token)\nSECRET_NAME=\nPATH=/usr/bin\nMY_APP_SECRET=s3cr3t-value\n";
        let (out, n) = hidden(env);
        assert_eq!(n, 3, "{out}");
        assert!(out.contains("HOME=/Users/sam") && out.contains("PATH=/usr/bin"));
        assert!(out.contains("export DB_PASSWORD=[redacted: secret]") && out.contains("API_KEY='[redacted: secret]'"));
        assert!(out.contains("MY_APP_SECRET=[redacted: secret]"));
        // A value that is a command or a variable gives nothing away.
        assert!(out.contains("GITHUB_TOKEN=$(gh auth token)"));

        // The same lines as the agent's own tools show them: numbered, or as search results.
        let (numbered, n) = hidden("    1  NAME=demo\n    2  DB_PASSWORD=correct-horse-battery\n");
        assert_eq!((numbered.as_str(), n), ("    1  NAME=demo\n    2  DB_PASSWORD=[redacted: secret]\n", 1));
        let (found, n) = hidden("config/app.env:7: SESSION_SECRET=0f9e8d7c6b5a\n");
        assert_eq!((found.as_str(), n), ("config/app.env:7: SESSION_SECRET=[redacted: secret]\n", 1));
        // And on a command line.
        let (cli, n) = hidden("mysql -u root --password=hunter2secret mydb");
        assert_eq!((cli.as_str(), n), ("mysql -u root --password=[redacted: secret] mydb", 1));
    }

    #[test]
    fn settings_files_lose_generated_values_but_code_is_left_readable() {
        let (out, n) = hidden("api_key: \"Zx9Kq2LmN8pQ4rS7tU1vW3yA5bC6dE\"\nclient_secret = aB3dE5fG7hI9jK1lM3nO5pQ7\n");
        assert_eq!(n, 2, "{out}");
        assert!(!out.contains("Zx9Kq2") && !out.contains("aB3dE5"));

        let code = "token = get_token()\npassword: str = \"\"\napi_key = os.environ[\"SERVICE_API_KEY\"]\nself.secret = secret\nif password == confirm_password:\n    token_count = len(tokens)\nAUTH_TOKEN_HEADER = \"X-Auth-Token\"\nsecret_description: thisisalongwordwithoutdigits\ndb = connect(host, password=db_password)\nclient = Client(\n    api_key=settings.key,\n    token=token_value,\n)\nusage: tool --token=<token> [--password=$PASS]\n";
        let (out, n) = hidden(code);
        assert_eq!((out.as_str(), n), (code, 0));
    }

    #[test]
    fn astrums_own_keys_are_removed_wherever_they_appear() {
        let known = vec!["my-provider-key-12345".to_string(), "short".to_string()];
        let (out, n) = scrub("echoed my-provider-key-12345 twice: my-provider-key-12345; short stays", &known);
        assert_eq!(n, 2);
        assert!(!out.contains("my-provider-key") && out.contains("short stays"));
        // Running it again finds nothing more.
        assert_eq!(scrub(&out, &known), (out.clone(), 0));
    }

    #[test]
    fn ordinary_output_is_untouched() {
        let text = "total 8\n-rw-r--r--  1 sam  staff  120 Nov 11 00:10 notes.txt\ncommit 3f2a9c1e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a39\nhttps://example.com/path?query=1\nuser@host:~/proj$ ls\n";
        assert_eq!(hidden(text), (text.to_string(), 0));
    }
}
