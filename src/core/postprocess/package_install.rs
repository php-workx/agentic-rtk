//! Package install log compression that preserves security-relevant lines.

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref SECURITY_RE: Regex = Regex::new(
        r"(?i)(vulnerabilit|security|critical|high severity|breaking|CVE-|GHSA-|malware)"
    )
    .unwrap();
    static ref DEPRECATED_RE: Regex =
        Regex::new(r"(?i)(npm warn deprecated|WARN deprecated|deprecat)").unwrap();
    static ref FUNDING_RE: Regex =
        Regex::new(r"(?i)(packages? (are|is) looking for funding|run .+fund)").unwrap();
    static ref SUMMARY_RE: Regex =
        Regex::new(r"(?i)(added|removed|changed|audited)\s+\d+\s+packages?.*?(?:in\s+\d+(?:\.\d+)?s)?").unwrap();
    static ref NPM_ADDED_RE: Regex =
        Regex::new(r"(?i)added\s+(\d+)\s+packages?.*?in\s+(\d+(?:\.\d+)?)s").unwrap();
    static ref VULN_SUMMARY_RE: Regex =
        Regex::new(r"(?i)(\d+)\s+vulnerabilit(?:y|ies)\s*(?:\(([^)]+)\))?").unwrap();
    static ref PROGRESS_RE: Regex =
        Regex::new(r"(?i)(^\s*progress:|^\s*[|/\\-]\s*$|\[[#=.>\s-]+\]\s*\d+%|\b\d+%\b|^\s*downloaded\s)").unwrap();
    static ref PIP_NOISE_RE: Regex =
        Regex::new(r"(?i)(already satisfied|using cached|downloading .+\.(whl|tar\.gz)|installing build dependencies)").unwrap();
    static ref CARGO_COMPILE_RE: Regex =
        Regex::new(r"^\s*(Compiling|Checking|Downloaded|Downloading)\s+").unwrap();
}

pub fn compress_pkg_log(input: &str) -> String {
    if !is_pkg_output(input) {
        return input.to_string();
    }

    let mut kept = Vec::new();
    let mut summary = None;
    let mut vuln_summary = None;
    let mut removed_noise = false;

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            removed_noise = true;
            continue;
        }

        if SUMMARY_RE.is_match(trimmed) {
            removed_noise = true;
            summary = Some(format_summary(trimmed));
            continue;
        }

        if SECURITY_RE.is_match(trimmed) || trimmed.to_lowercase().contains("audit") {
            if let Some(caps) = VULN_SUMMARY_RE.captures(trimmed) {
                let count = caps.get(1).map(|m| m.as_str()).unwrap_or("?");
                let details = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                vuln_summary = if details.is_empty() {
                    Some(format!("[security] {} vulnerabilities", count))
                } else {
                    Some(format!(
                        "[security] {} vulnerabilities ({})",
                        count, details
                    ))
                };
            } else {
                kept.push(format!("[security] {}", trimmed));
            }
            continue;
        }

        if DEPRECATED_RE.is_match(trimmed)
            || FUNDING_RE.is_match(trimmed)
            || PROGRESS_RE.is_match(trimmed)
            || PIP_NOISE_RE.is_match(trimmed)
            || CARGO_COMPILE_RE.is_match(trimmed)
        {
            removed_noise = true;
            continue;
        }

        kept.push(line.to_string());
    }

    let mut result = Vec::new();
    if let Some(summary) = summary {
        result.push(summary);
    }
    if let Some(vuln_summary) = vuln_summary {
        result.push(vuln_summary);
    }
    result.extend(kept);

    if result.is_empty() || (!removed_noise && result.join("\n") == input.trim()) {
        input.to_string()
    } else {
        result.join("\n")
    }
}

fn format_summary(line: &str) -> String {
    if let Some(caps) = NPM_ADDED_RE.captures(line) {
        let count = caps.get(1).map(|m| m.as_str()).unwrap_or("?");
        let seconds = caps.get(2).map(|m| m.as_str()).unwrap_or("?");
        format!("ok {} packages ({}s)", count, seconds)
    } else {
        line.to_string()
    }
}

fn is_pkg_output(input: &str) -> bool {
    let lower = input.to_lowercase();
    [
        "npm warn",
        "added ",
        " packages",
        "looking for funding",
        "vulnerabilit",
        "already satisfied",
        "using cached",
        "downloading",
        "installing collected",
        "successfully installed",
        "compiling ",
        "deprecated",
        "audited",
        "installed package",
    ]
    .iter()
    .any(|indicator| lower.contains(indicator))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_install_removes_non_security_deprecated_and_funding() {
        let input = r#"npm warn deprecated rimraf@3.0.2: Rimraf versions prior to v4 are no longer supported
added 847 packages, and audited 848 packages in 32s
143 packages are looking for funding
  run `npm fund` for details
8 vulnerabilities (6 high, 2 moderate)"#;

        let result = compress_pkg_log(input);
        assert!(!result.contains("rimraf"));
        assert!(!result.contains("looking for funding"));
        assert!(result.contains("ok 847 packages (32s)"));
        assert!(result.contains("[security] 8 vulnerabilities (6 high, 2 moderate)"));
    }

    #[test]
    fn security_deprecated_lines_are_preserved() {
        let input = r#"npm warn deprecated bcrypt@3.0.0: security vulnerability (CVE-2023-31484)
npm warn deprecated old-util@1.0.0: Use new-util instead
added 100 packages in 5s"#;

        let result = compress_pkg_log(input);
        assert!(result.contains("CVE-2023-31484"));
        assert!(result.contains("bcrypt@3.0.0"));
        assert!(!result.contains("old-util"));
    }

    #[test]
    fn ghsa_and_audit_lines_are_preserved() {
        let input = r#"npm audit report
some-pkg has a known security issue GHSA-abcd-1234-efgh
added 50 packages in 3s"#;

        let result = compress_pkg_log(input);
        assert!(result.contains("GHSA-abcd-1234-efgh"));
        assert!(result.contains("[security]"));
    }

    #[test]
    fn pip_cache_noise_is_removed() {
        let input = r#"Requirement already satisfied: requests in /usr/lib/python3/dist-packages
Using cached certifi-2023.7.22-py3-none-any.whl (158 kB)
Successfully installed flask-2.3.3"#;

        let result = compress_pkg_log(input);
        assert!(!result.to_lowercase().contains("already satisfied"));
        assert!(!result.contains("Using cached"));
        assert!(result.contains("Successfully installed flask-2.3.3"));
    }

    #[test]
    fn normal_output_passes_through() {
        let input = "Hello world\nNo package patterns here\n";
        assert_eq!(compress_pkg_log(input), input);
    }
}
