//! Port of GokuCore's `Redaction`: secrets never reach logs, errors or endpoint summaries. Keep the patterns identical to
//! the Goku app's `Redaction.swift` (fancy-regex, because the dollar-quote rule needs a backreference).
use fancy_regex::Regex;
use std::sync::LazyLock;

const QUOTED: &str = r#"(?:'(?:[^'\\]|''|\\.)*'|"(?:[^"\\]|\\.)*")"#;
const SQL_QUOTED: &str = r#"(?:'(?:[^'\\]|''|\\.)*'|"(?:[^"\\]|\\.)*"|\$(?<tag>\w*)\$[\s\S]*?\$\k<tag>\$)"#;
const SECRET_PARAMS: &str = "password|passwd|pwd|sslpassword|tlsCertificateKeyFilePassword|tlsCAFilePassword";

#[expect(clippy::expect_used, reason = "the patterns are constants, so the unit tests cover every one")]
fn rule(pattern: String, template: &'static str) -> (Regex, &'static str) {
    (Regex::new(&format!("(?i){pattern}")).expect("redaction pattern"), template)
}

fn uri_rules() -> Vec<(Regex, &'static str)> {
    vec![
        // Greedy to the last `@`, so a password with an unencoded `@`, `/`, `?` or `#` is still removed whole.
        rule(r#"(://[^:/\s@'"]+:)[^\s'"]*@"#.into(), "${1}****@"),
        rule(format!(r#"((?:^|[?&;\s])(?:{SECRET_PARAMS})=)[^&\s'";]+"#), "${1}****"),
    ]
}

static URI_RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(uri_rules);

static STATEMENT_RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let mut rules = vec![
        // PASSWORD 'x', PASSWORD $$x$$, SET PASSWORD [FOR u] = [PASSWORD(]'x', pwd: "x", "password": "x"
        rule(
            format!(
                r#"(["'`]?\b(?:password|passwd|pwd)\b["'`]?(?:\s+FOR\s+\S+)?\s*[:=]?\s*(?:PASSWORD\s*\(\s*)?){SQL_QUOTED}"#
            ),
            "${1}'***'",
        ),
        // MySQL: IDENTIFIED [WITH plugin] BY 'x'
        rule(format!(r#"(\bIDENTIFIED\b[^;'"]*?\bBY\s+){SQL_QUOTED}"#), "${1}'***'"),
        // mongo shell: db.changeUserPassword('user', 'x')
        rule(format!(r#"(\bchangeUserPassword\s*\(\s*{QUOTED}\s*,\s*){QUOTED}"#), "${1}'***'"),
    ];
    rules.extend(uri_rules());
    rules
});

fn apply(rules: &[(Regex, &'static str)], s: &str) -> String {
    rules.iter().fold(s.to_owned(), |acc, (re, template)| re.replace_all(&acc, *template).into_owned())
}

/// Passwords in statements, commands, connection strings and error messages become `***` / `****`.
pub fn secrets(s: &str) -> String {
    apply(&STATEMENT_RULES, s)
}

/// Only the connection-string rules (for URIs shown as endpoints).
pub fn uri(s: &str) -> String {
    apply(&URI_RULES, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_password_is_removed_up_to_the_last_at() {
        assert_eq!(uri("mongodb://u:p@ss@h:27017/db"), "mongodb://u:****@h:27017/db");
        assert_eq!(uri("postgres://u@h/db"), "postgres://u@h/db");
        assert_eq!(uri("mysql://h/db?user=u&password=s3cret&ssl=1"), "mysql://h/db?user=u&password=****&ssl=1");
        assert_eq!(uri("x?PWD=abc"), "x?PWD=****");
    }

    #[test]
    fn statement_secrets() {
        assert_eq!(secrets("ALTER USER bob PASSWORD 'x''y'"), "ALTER USER bob PASSWORD '***'");
        assert_eq!(secrets("ALTER ROLE r PASSWORD $tag$se$cret$tag$"), "ALTER ROLE r PASSWORD '***'");
        assert_eq!(
            secrets("CREATE USER u IDENTIFIED WITH caching_sha2_password BY 'pw'"),
            "CREATE USER u IDENTIFIED WITH caching_sha2_password BY '***'"
        );
        assert_eq!(secrets("db.changeUserPassword('u', \"pw\")"), "db.changeUserPassword('u', '***')");
        assert_eq!(secrets(r#"{"password": "pw"}"#), r#"{"password": '***'}"#);
        assert_eq!(secrets("failed for postgres://a:b@h"), "failed for postgres://a:****@h");
    }
}
