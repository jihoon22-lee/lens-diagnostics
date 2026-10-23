//! PEP 508 environment-marker evaluation and PEP 440 version
//! specifier matching for `Requires-Dist` lines.
//!
//! Evaluation is three-state: a marker that references a variable we
//! cannot know statically (or that fails to parse) reports
//! `Unknown` — never silently assumed true or false.

use std::cmp::Ordering;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriState {
    True,
    False,
    Unknown,
    Error,
}

impl TriState {
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Error, _) | (_, Self::Error) => Self::Error,
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }

    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Error, _) | (_, Self::Error) => Self::Error,
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }
}

/// Marker evaluation context derived from pyvenv.cfg plus the host
/// platform lens runs on.
#[derive(Debug, Clone)]
pub struct MarkerEnv {
    pub python_version: String,
    pub python_full_version: String,
    pub os_name: String,
    pub sys_platform: String,
    pub platform_system: String,
    pub platform_machine: String,
    pub platform_python_implementation: String,
    pub implementation_name: String,
    pub implementation_version: String,
    /// Normalized extra names requested via `--extras`.
    pub extras: BTreeSet<String>,
}

impl MarkerEnv {
    pub fn for_venv(python_full_version: &str, extras: &[String]) -> Self {
        let mut short = python_full_version.split('.');
        let python_version = match (short.next(), short.next()) {
            (Some(maj), Some(min)) => format!("{}.{}", maj, min),
            _ => python_full_version.to_string(),
        };
        let sys_platform = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        };
        let platform_system = match std::env::consts::OS {
            "macos" => "Darwin".to_string(),
            "windows" => "Windows".to_string(),
            other => {
                let mut c = other.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => other.to_string(),
                }
            }
        };
        Self {
            python_version,
            python_full_version: python_full_version.to_string(),
            os_name: if cfg!(windows) { "nt" } else { "posix" }.to_string(),
            sys_platform: sys_platform.to_string(),
            platform_system,
            platform_machine: std::env::consts::ARCH.to_string(),
            platform_python_implementation: "CPython".to_string(),
            implementation_name: "cpython".to_string(),
            implementation_version: python_full_version.to_string(),
            extras: extras
                .iter()
                .map(|e| normalize_extra(e))
                .filter(|e| !e.is_empty())
                .collect(),
        }
    }

    fn var(&self, name: &str) -> Option<&str> {
        match name {
            "python_version" => Some(&self.python_version),
            "python_full_version" => Some(&self.python_full_version),
            "os_name" => Some(&self.os_name),
            "sys_platform" => Some(&self.sys_platform),
            "platform_system" => Some(&self.platform_system),
            "platform_machine" => Some(&self.platform_machine),
            "platform_python_implementation" => Some(&self.platform_python_implementation),
            "implementation_name" => Some(&self.implementation_name),
            "implementation_version" => Some(&self.implementation_version),
            _ => None,
        }
    }
}

pub fn normalize_extra(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], ".")
        .to_string()
}

// --- tokenizer ---

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    LParen,
    RParen,
    Op(&'static str),
}

fn tokenize(input: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = input.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            '\'' | '"' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != c {
                    j += 1;
                }
                if j >= chars.len() {
                    return None;
                }
                toks.push(Tok::Str(chars[i + 1..j].iter().collect()));
                i = j + 1;
            }
            '=' | '!' | '<' | '>' | '~' => {
                let rest: String = chars[i..chars.len().min(i + 3)].iter().collect();
                let (op, len) = if rest.starts_with("===") {
                    ("===", 3)
                } else if rest.starts_with("==") {
                    ("==", 2)
                } else if rest.starts_with("!=") {
                    ("!=", 2)
                } else if rest.starts_with("~=") {
                    ("~=", 2)
                } else if rest.starts_with("<=") {
                    ("<=", 2)
                } else if rest.starts_with(">=") {
                    (">=", 2)
                } else if c == '<' {
                    ("<", 1)
                } else if c == '>' {
                    (">", 1)
                } else {
                    return None;
                };
                toks.push(Tok::Op(op));
                i += len;
            }
            _ if c.is_alphanumeric() || c == '_' || c == '.' || c == '*' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric()
                        || chars[i] == '_'
                        || chars[i] == '.'
                        || chars[i] == '*')
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match word.as_str() {
                    "and" | "or" | "not" | "in" => toks.push(Tok::Op(match word.as_str() {
                        "and" => "and",
                        "or" => "or",
                        "not" => "not",
                        _ => "in",
                    })),
                    _ => toks.push(Tok::Ident(word)),
                }
            }
            _ => return None,
        }
    }
    Some(toks)
}

// --- parser ---

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<&Tok> {
        let t = self.toks.get(self.pos);
        self.pos += 1;
        t
    }

    /// or_expr := and_expr ("or" and_expr)*
    fn parse_or(&mut self, env: &MarkerEnv) -> TriState {
        let mut acc = self.parse_and(env);
        while matches!(self.peek(), Some(Tok::Op("or"))) {
            self.next();
            acc = acc.or(self.parse_and(env));
        }
        acc
    }

    /// and_expr := cmp_expr ("and" cmp_expr)*
    fn parse_and(&mut self, env: &MarkerEnv) -> TriState {
        let mut acc = self.parse_cmp(env);
        while matches!(self.peek(), Some(Tok::Op("and"))) {
            self.next();
            acc = acc.and(self.parse_cmp(env));
        }
        acc
    }

    /// cmp_expr := "(" or_expr ")" | operand op operand
    fn parse_cmp(&mut self, env: &MarkerEnv) -> TriState {
        if matches!(self.peek(), Some(Tok::LParen)) {
            self.next();
            let v = self.parse_or(env);
            if matches!(self.next(), Some(Tok::RParen)) {
                return v;
            }
            return TriState::Error;
        }
        let lhs = self.parse_operand();
        let op = self.parse_op();
        let rhs = self.parse_operand();
        match (lhs, op, rhs) {
            (Some(l), Some(o), Some(r)) => eval_comparison(&l, &o, &r, env),
            _ => TriState::Error,
        }
    }

    fn parse_operand(&mut self) -> Option<Operand> {
        match self.next() {
            Some(Tok::Ident(w)) => {
                if is_marker_var(w) {
                    Some(Operand::Var(w.clone()))
                } else {
                    // Lenient: an unquoted bareword is treated as a literal.
                    Some(Operand::Lit(w.clone()))
                }
            }
            Some(Tok::Str(s)) => Some(Operand::Lit(s.clone())),
            _ => {
                self.pos = self.pos.saturating_sub(1);
                None
            }
        }
    }

    fn parse_op(&mut self) -> Option<String> {
        match self.next() {
            Some(Tok::Op("not")) => {
                if matches!(self.next(), Some(Tok::Op("in"))) {
                    Some("not in".to_string())
                } else {
                    None
                }
            }
            Some(Tok::Op(o)) if !matches!(*o, "and" | "or") => Some(o.to_string()),
            _ => {
                self.pos = self.pos.saturating_sub(1);
                None
            }
        }
    }
}

#[derive(Debug)]
enum Operand {
    Var(String),
    Lit(String),
}

fn is_marker_var(name: &str) -> bool {
    matches!(
        name,
        "python_version"
            | "python_full_version"
            | "os_name"
            | "sys_platform"
            | "platform_system"
            | "platform_machine"
            | "platform_version"
            | "platform_release"
            | "platform_python_implementation"
            | "implementation_name"
            | "implementation_version"
            | "extra"
    )
}

fn eval_comparison(lhs: &Operand, op: &str, rhs: &Operand, env: &MarkerEnv) -> TriState {
    // Normalize so the marker variable is on the left.
    let (var, val, op) = match (lhs, rhs) {
        (Operand::Var(v), Operand::Lit(s)) => (v.as_str(), s.as_str(), op.to_string()),
        (Operand::Lit(s), Operand::Var(v)) => (v.as_str(), s.as_str(), flip_op(op)),
        (Operand::Var(a), Operand::Var(b)) => {
            // var == var — only sane as name equality of literals.
            return if a == b && (op == "==" || op == "===") {
                TriState::True
            } else {
                TriState::Unknown
            };
        }
        (Operand::Lit(_), Operand::Lit(_)) => return TriState::Error,
    };

    if var == "extra" {
        return eval_extra(op.as_str(), val, env);
    }

    let Some(actual) = env.var(var) else {
        return TriState::Unknown;
    };
    compare_value(actual, op.as_str(), val, is_version_var(var))
}

fn flip_op(op: &str) -> String {
    match op {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        "in" => "contains",
        "not in" => "not contains",
        other => other,
    }
    .to_string()
}

fn is_version_var(name: &str) -> bool {
    matches!(
        name,
        "python_version"
            | "python_full_version"
            | "implementation_version"
            | "platform_version"
            | "platform_release"
    )
}

fn eval_extra(op: &str, val: &str, env: &MarkerEnv) -> TriState {
    let want = normalize_extra(val);
    // PEP 508: the marker is evaluated once per requested extra; when
    // none are requested it is evaluated against the empty string.
    let candidates: Vec<&str> = if env.extras.is_empty() {
        vec![""]
    } else {
        env.extras.iter().map(|e| e.as_str()).collect()
    };
    let mut saw_unknown = false;
    let mut saw_true = false;
    for e in candidates {
        match compare_value(e, op, &want, false) {
            TriState::True => saw_true = true,
            TriState::Unknown | TriState::Error => saw_unknown = true,
            TriState::False => {}
        }
    }
    if saw_true {
        TriState::True
    } else if saw_unknown {
        TriState::Unknown
    } else {
        TriState::False
    }
}

fn compare_value(actual: &str, op: &str, expected: &str, version: bool) -> TriState {
    match op {
        "in" | "not in" => {
            // PEP 508: `in`/`not in` are string containment for every
            // variable (version-valued ones included) — never version-token
            // equality, which would wrongly reject e.g. '3.10' in '3.10.1 3.11'.
            let matched = expected.contains(actual);
            return tri(if op == "in" { matched } else { !matched });
        }
        // flipped forms from flip_op
        "contains" => return tri(actual.contains(expected)),
        "not contains" => return tri(!actual.contains(expected)),
        _ => {}
    }

    if version || op == "~=" {
        return version_satisfies(actual, &format!("{op}{expected}"))
            .map(tri)
            .unwrap_or(TriState::Unknown);
    }
    let ord = actual.cmp(expected);
    match op {
        "==" | "===" => tri(ord == Ordering::Equal),
        "!=" => tri(ord != Ordering::Equal),
        "<" => tri(ord == Ordering::Less),
        "<=" => tri(ord != Ordering::Greater),
        ">" => tri(ord == Ordering::Greater),
        ">=" => tri(ord != Ordering::Less),
        _ => TriState::Unknown,
    }
}

fn tri(b: bool) -> TriState {
    if b {
        TriState::True
    } else {
        TriState::False
    }
}

/// Evaluate a PEP 508 marker expression against the venv context.
/// Returns `Unknown` when the expression cannot be parsed or mentions
/// variables lens cannot determine.
pub fn evaluate_marker(expr: &str, env: &MarkerEnv) -> TriState {
    let Some(toks) = tokenize(expr) else {
        return TriState::Unknown;
    };
    if toks.is_empty() {
        return TriState::True;
    }
    let mut p = Parser { toks, pos: 0 };
    let v = p.parse_or(env);
    if p.pos != p.toks.len() || v == TriState::Error {
        return TriState::Unknown;
    }
    v
}

// --- requirement handling ---

pub enum ReqVerdict {
    /// Marker evaluated false — the dependency does not apply.
    Inactive,
    /// Marker applies; `name` is the normalized package name and
    /// `spec` is the version specifier string (may be empty).
    Active { name: String, spec: String },
    /// Marker or requirement could not be parsed/evaluated.
    Unevaluated,
}

/// Split `Requires-Dist` into its head (name/spec) and optional
/// marker, honoring `;` inside quoted marker strings.
fn split_marker(req: &str) -> (&str, Option<&str>) {
    let mut quote = None;
    for (i, c) in req.char_indices() {
        match c {
            '\'' | '"' => {
                if quote == Some(c) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(c);
                }
            }
            ';' if quote.is_none() => return (&req[..i], Some(&req[i + 1..])),
            _ => {}
        }
    }
    (req, None)
}

/// Strip optional outer parentheses from a version specifier string
/// (e.g. `(>=1.21.1,<3)` -> `>=1.21.1,<3`). Returns `None` if parentheses
/// are unbalanced.
fn strip_spec_parens(spec: &str) -> Option<&str> {
    let spec = spec.trim();
    if spec.starts_with('(') || spec.ends_with(')') {
        spec.strip_prefix('(')?.strip_suffix(')').map(str::trim)
    } else {
        Some(spec)
    }
}

/// Evaluate a full `Requires-Dist` value: `name[extras] spec ; marker`.
pub fn eval_requirement(req: &str, env: &MarkerEnv) -> ReqVerdict {
    let (head, marker) = split_marker(req);
    if let Some(m) = marker {
        match evaluate_marker(m.trim(), env) {
            TriState::False => return ReqVerdict::Inactive,
            TriState::Unknown | TriState::Error => return ReqVerdict::Unevaluated,
            TriState::True => {}
        }
    }

    let head = head.trim();
    let name: String = head
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if name.is_empty() {
        return ReqVerdict::Unevaluated;
    }
    let mut rest = &head[name.len()..];
    if rest.starts_with('[') {
        match rest.find(']') {
            Some(end) => rest = &rest[end + 1..],
            None => return ReqVerdict::Unevaluated,
        }
    }
    let Some(spec) = strip_spec_parens(rest) else {
        return ReqVerdict::Unevaluated;
    };
    ReqVerdict::Active {
        name,
        spec: spec.to_string(),
    }
}

/// Does `installed` satisfy a PEP 440 version specifier like
/// `>=3.1,<4` or `(>=1.21.1,<3)`? `None` when the specifier cannot be evaluated.
pub fn version_satisfies(installed: &str, spec: &str) -> Option<bool> {
    let spec = strip_spec_parens(spec)?;
    if spec.is_empty() {
        return Some(true);
    }
    let version = installed.parse::<pep440_rs::Version>().ok();
    let mut matched = true;
    for clause in spec.split(',') {
        let (op, expected) = split_op(clause.trim())?;
        if expected.is_empty() {
            return None;
        }
        let satisfies = if op == "===" {
            // Arbitrary equality must also support non-PEP-440 vendor strings.
            installed.eq_ignore_ascii_case(expected)
        } else {
            let specifier = format!("{op}{expected}")
                .parse::<pep440_rs::VersionSpecifier>()
                .ok()?;
            matches_specifier(version.as_ref()?, &specifier)
        };
        // Validate every clause even after one mismatches: malformed evidence
        // must not masquerade as a known version conflict.
        matched &= satisfies;
    }
    Some(matched)
}

/// Use the library's complete version parser and ordering, with PEP 440's
/// specifier rules applied explicitly. In pep440_rs 0.7.3, `contains` rejects
/// same-release prereleases even for prerelease `<` bounds, and wildcard
/// matching does not zero-pad short candidate release segments.
fn matches_specifier(version: &pep440_rs::Version, spec: &pep440_rs::VersionSpecifier) -> bool {
    use pep440_rs::Operator;

    let bound = spec.version();
    let public = version.clone().without_local();
    let candidate = if bound.is_local() { version } else { &public };
    let prefix_matches = |prefix: &[u64]| {
        version.epoch() == bound.epoch()
            && prefix
                .iter()
                .enumerate()
                .all(|(i, part)| version.release().get(i).copied().unwrap_or(0) == *part)
    };
    let same_base = version.epoch() == bound.epoch()
        && (0..version.release().len().max(bound.release().len())).all(|i| {
            version.release().get(i).copied().unwrap_or(0)
                == bound.release().get(i).copied().unwrap_or(0)
        });
    match spec.operator() {
        Operator::Equal => candidate == bound,
        Operator::NotEqual => candidate != bound,
        Operator::EqualStar => prefix_matches(bound.release()),
        Operator::NotEqualStar => !prefix_matches(bound.release()),
        Operator::TildeEqual => {
            candidate >= bound && prefix_matches(&bound.release()[..bound.release().len() - 1])
        }
        Operator::LessThan => {
            candidate < bound && !(!bound.any_prerelease() && version.any_prerelease() && same_base)
        }
        Operator::GreaterThan => {
            candidate > bound
                && !(!bound.is_post() && version.is_post() && same_base)
                && !(version.is_local() && same_base)
        }
        Operator::LessThanEqual => candidate <= bound,
        Operator::GreaterThanEqual => candidate >= bound,
        #[allow(deprecated)]
        Operator::ExactEqual => unreachable!("arbitrary equality uses the original strings"),
    }
}

fn split_op(clause: &str) -> Option<(&'static str, &str)> {
    for op in ["===", "==", "!=", "~=", ">=", "<="] {
        if let Some(v) = clause.strip_prefix(op) {
            return Some((op, v.trim()));
        }
    }
    if let Some(v) = clause.strip_prefix('>') {
        return Some((">", v.trim()));
    }
    if let Some(v) = clause.strip_prefix('<') {
        return Some(("<", v.trim()));
    }
    // Bare version means `==`.
    if clause
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        Some(("==", clause))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pep440_matches_pypa_reference_matrix() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pep440-reference.json")).unwrap();
        let versions = fixture["versions"].as_array().unwrap();
        let specifiers = fixture["specifiers"].as_array().unwrap();
        let matches = fixture["matches"].as_array().unwrap();
        assert_eq!(versions.len(), matches.len());
        for (version, expected) in versions.iter().zip(matches) {
            let version = version.as_str().unwrap();
            let expected = expected.as_str().unwrap();
            assert_eq!(specifiers.len(), expected.len());
            for (spec, expected) in specifiers.iter().zip(expected.bytes()) {
                let spec = spec.as_str().unwrap();
                assert_eq!(
                    version_satisfies(version, spec),
                    Some(expected == b'1'),
                    "{version} {spec}"
                );
            }
        }
    }

    #[test]
    fn compound_versions_match_installed_version_specifier_semantics() {
        // Expected results cross-checked against Python packaging with
        // prereleases=True: these are installed versions, not index candidates.
        for (installed, spec, expected) in [
            ("1.0rc1.dev1", "==1.0rc1", false),
            ("1.0rc1.dev1", "<1.0rc1", true),
            ("1.0rc1", "==1.0rc1", true),
            ("1.0.post1.dev1", "==1.0.post1", false),
            ("1.0.post1.dev1", "<1.0.post1", false),
            ("1.0.post1.dev1", "<=1.0.post1", true),
            ("1.0rc1", "<1.0", false),
            ("1.0rc1", "<2.0", true),
            ("1!0.2", ">=1!0.1,<1!1.0", true),
            ("1.0+local", "==1.0", true),
            ("1.0+local", "==1.0+other", false),
            ("1.0rc1", "==1.*", true),
            ("vendor-build", "===vendor-build", true),
        ] {
            assert_eq!(
                version_satisfies(installed, spec),
                Some(expected),
                "{installed} {spec}"
            );
        }
        assert_eq!(version_satisfies("1.0rc1.nonsense", "==1.0rc1"), None);
        assert_eq!(version_satisfies("1.0", "<0.5,not-a-specifier"), None);
        assert_eq!(version_satisfies("1.0", "=="), None);
        let env = MarkerEnv::for_venv("3.14.0rc1.dev1", &[]);
        assert_eq!(
            evaluate_marker("python_full_version == '3.14.0rc1'", &env),
            TriState::False
        );
    }

    fn env() -> MarkerEnv {
        MarkerEnv::for_venv("3.14.4", &[])
    }

    fn env_with(extras: &[&str]) -> MarkerEnv {
        MarkerEnv::for_venv(
            "3.14.4",
            &extras.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn test_version_markers() {
        let e = env();
        assert_eq!(
            evaluate_marker("python_version < '3.10'", &e),
            TriState::False
        );
        assert_eq!(
            evaluate_marker("python_version >= '3.10'", &e),
            TriState::True
        );
        assert_eq!(
            evaluate_marker("python_full_version >= '3.14.0'", &e),
            TriState::True
        );
        assert_eq!(
            evaluate_marker("python_version == '3.14.*'", &e),
            TriState::True
        );
    }

    #[test]
    fn test_platform_markers() {
        let e = env();
        assert_eq!(
            evaluate_marker("sys_platform == \"linux\"", &e),
            TriState::True
        );
        assert_eq!(
            evaluate_marker("sys_platform == \"win32\"", &e),
            TriState::False
        );
        assert_eq!(evaluate_marker("os_name == 'posix'", &e), TriState::True);
        assert_eq!(
            evaluate_marker("platform_system == 'Windows'", &e),
            TriState::False
        );
    }

    #[test]
    fn test_compound_markers() {
        let e = env();
        assert_eq!(
            evaluate_marker("sys_platform == 'linux' and python_version >= '3.8'", &e),
            TriState::True
        );
        assert_eq!(
            evaluate_marker(
                "(sys_platform == 'win32' or sys_platform == 'linux') and python_version < '3.8'",
                &e
            ),
            TriState::False
        );
        assert_eq!(
            evaluate_marker("python_version < '3.0' or python_version >= '3.0'", &e),
            TriState::True
        );
    }

    #[test]
    fn test_extra_markers() {
        let e = env();
        assert_eq!(evaluate_marker("extra == 'testing'", &e), TriState::False);
        let e2 = env_with(&["testing"]);
        assert_eq!(evaluate_marker("extra == 'testing'", &e2), TriState::True);
        assert_eq!(
            evaluate_marker("sys_platform == 'linux' and extra == 'testing'", &e2),
            TriState::True
        );
        // PEP 508 normalization: "my-extra" == "my.extra" == "my_extra"
        let e3 = env_with(&["my-extra"]);
        assert_eq!(evaluate_marker("extra == 'my.extra'", &e3), TriState::True);
    }

    #[test]
    fn test_unknown_marker_vars() {
        let e = env();
        // platform_release is not determinable statically.
        assert_eq!(
            evaluate_marker("platform_release >= '6.0'", &e),
            TriState::Unknown
        );
        // or-shortcircuit: True branch still resolves.
        assert_eq!(
            evaluate_marker("sys_platform == 'linux' or platform_release > '9'", &e),
            TriState::True
        );
        // and: False dominates unknown.
        assert_eq!(
            evaluate_marker("sys_platform == 'win32' and platform_release > '9'", &e),
            TriState::False
        );
        assert_eq!(evaluate_marker("garbage (((", &e), TriState::Unknown);
    }

    #[test]
    fn test_tristate_error_not_swallowed_by_shortcircuit() {
        let e = env();
        assert_eq!(TriState::True.or(TriState::Error), TriState::Error);
        assert_eq!(TriState::Error.or(TriState::True), TriState::Error);
        assert_eq!(TriState::False.and(TriState::Error), TriState::Error);
        assert_eq!(TriState::Error.and(TriState::False), TriState::Error);

        // Malformed marker syntax on RHS of True `or` or False `and` must fail closed (Unknown).
        assert_eq!(
            evaluate_marker("sys_platform == 'linux' or (", &e),
            TriState::Unknown
        );
        assert_eq!(
            evaluate_marker("sys_platform == 'win32' and ==", &e),
            TriState::Unknown
        );
    }

    #[test]
    fn test_version_in_and_not_in_markers() {
        let e_311 = MarkerEnv::for_venv("3.11.8", &[]);
        let e_314 = MarkerEnv::for_venv("3.14.4", &[]);
        let e_31 = MarkerEnv::for_venv("3.1.0", &[]);

        assert_eq!(
            evaluate_marker("python_version in '3.10 3.11 3.12'", &e_311),
            TriState::True
        );
        assert_eq!(
            evaluate_marker("python_version in '3.10 3.11 3.12'", &e_314),
            TriState::False
        );
        assert_eq!(
            evaluate_marker("python_version not in '3.10 3.11 3.12'", &e_314),
            TriState::True
        );
        // PEP 508 `in` is plain string containment, so '3.1' is contained in
        // '3.10 3.11 3.12' (users wanting exact tokens must not rely on this).
        assert_eq!(
            evaluate_marker("python_version in '3.10 3.11 3.12'", &e_31),
            TriState::True
        );
        // Regression: a legal marker listing a longer patch version must
        // still match the major.minor `python_version` by containment.
        let e_310 = MarkerEnv::for_venv("3.10.4", &[]);
        assert_eq!(
            evaluate_marker("python_version in '3.10.1 3.11'", &e_310),
            TriState::True
        );
        assert_eq!(
            evaluate_marker("python_version not in '3.10.1 3.11'", &e_310),
            TriState::False
        );
    }

    #[test]
    fn test_parenthesized_specs_and_pep440_epochs_prereleases() {
        let e = MarkerEnv::for_venv("3.11.2", &[]);
        match eval_requirement(
            "urllib3 (>=1.21.1,<3) ; python_version in '3.10 3.11 3.12'",
            &e,
        ) {
            ReqVerdict::Active { name, spec } => {
                assert_eq!(name, "urllib3");
                assert_eq!(spec, ">=1.21.1,<3");
                assert_eq!(version_satisfies("2.0.7", &spec), Some(true));
            }
            _ => panic!("expected active"),
        }
        assert_eq!(version_satisfies("2.0.7", "(>=1.21.1,<3)"), Some(true));
        assert_eq!(version_satisfies("3.0.0", "(>=1.21.1,<3)"), Some(false));

        // PEP 440 epochs: 1!0.1.0 > 2.0.0
        assert_eq!(version_satisfies("1!0.1.0", ">=2.0.0"), Some(true));
        assert_eq!(version_satisfies("2.0.0", ">=1!0.1.0"), Some(false));
        assert_eq!(version_satisfies("1!0.2.0", ">=1!0.1.0,<1!1.0"), Some(true));

        // PEP 440 pre-releases: dev < a < b < rc < final < post
        assert_eq!(version_satisfies("1.0.0.dev1", "<1.0.0a1"), Some(true));
        assert_eq!(version_satisfies("1.0.0a1", "<1.0.0b1"), Some(true));
        assert_eq!(version_satisfies("1.0.0b1", "<1.0.0rc1"), Some(true));
        // Exclusive final-release bounds exclude prereleases of that release.
        assert_eq!(version_satisfies("1.0.0rc1", "<1.0.0"), Some(false));
        assert_eq!(version_satisfies("1.0.0", "<1.0.0.post1"), Some(true));
    }

    #[test]
    fn test_eval_requirement() {
        let e = env();
        match eval_requirement("importlib-metadata>=3.6.0; python_version < '3.10'", &e) {
            ReqVerdict::Inactive => {}
            _ => panic!("expected inactive"),
        }
        match eval_requirement("pytest>=9.0.3; extra == 'testing'", &e) {
            ReqVerdict::Inactive => {}
            _ => panic!("expected inactive"),
        }
        match eval_requirement("Werkzeug<4,>=3.1.9", &e) {
            ReqVerdict::Active { name, spec } => {
                assert_eq!(name, "Werkzeug");
                assert_eq!(spec, "<4,>=3.1.9");
            }
            _ => panic!("expected active"),
        }
        match eval_requirement(
            "httpx[http2]>=0.23.0; extra == 'testing'",
            &env_with(&["testing"]),
        ) {
            ReqVerdict::Active { name, spec } => {
                assert_eq!(name, "httpx");
                assert_eq!(spec, ">=0.23.0");
            }
            _ => panic!("expected active"),
        }
        match eval_requirement("pkg; platform_release > '9'", &e) {
            ReqVerdict::Unevaluated => {}
            _ => panic!("expected unevaluated"),
        }
    }

    #[test]
    fn test_version_satisfies() {
        assert_eq!(version_satisfies("3.1.9", ">=3.1,<4"), Some(true));
        assert_eq!(version_satisfies("4.0", "<4"), Some(false));
        assert_eq!(version_satisfies("1.2.3", "~=1.2.0"), Some(true));
        assert_eq!(version_satisfies("1.3.0", "~=1.2.0"), Some(false));
        assert_eq!(version_satisfies("2.0", "==2.0.*"), Some(true));
        assert_eq!(version_satisfies("2.1.4", "!=2.0"), Some(true));
        assert_eq!(version_satisfies("0.23.0", ""), Some(true));
    }
}
