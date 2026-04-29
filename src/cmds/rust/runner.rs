//! Runs arbitrary commands and captures only stderr or test failures.

use crate::core::postprocess::PostprocessKind;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::process::Command;

const NO_ERRORS_WARNINGS_MATCHED: &str = "[rtk] no errors/warnings matched";
const FAILURE_TAIL_LINES: usize = 8;

lazy_static! {
    static ref ERROR_PATTERNS: Vec<Regex> = vec![
        // Generic errors
        Regex::new(r"(?i)^.*error[\s:\[].*$").unwrap(),
        Regex::new(r"(?i)^.*\berr\b.*$").unwrap(),
        Regex::new(r"(?i)^.*warning[\s:\[].*$").unwrap(),
        Regex::new(r"(?i)^.*\bwarn\b.*$").unwrap(),
        Regex::new(r"(?i)^.*failed.*$").unwrap(),
        Regex::new(r"(?i)^.*failure.*$").unwrap(),
        Regex::new(r"(?i)^.*exception.*$").unwrap(),
        Regex::new(r"(?i)^.*panic.*$").unwrap(),
        // Rust specific
        Regex::new(r"^error\[E\d+\]:.*$").unwrap(),
        Regex::new(r"^\s*--> .*:\d+:\d+$").unwrap(),
        // Python
        Regex::new(r"^Traceback.*$").unwrap(),
        Regex::new(r#"^\s*File ".*", line \d+.*$"#).unwrap(),
        // JavaScript/TypeScript
        Regex::new(r"^\s*at .*:\d+:\d+.*$").unwrap(),
        // Go
        Regex::new(r"^.*\.go:\d+:.*$").unwrap(),
    ];
}

fn build_shell_command(command: &str) -> Command {
    if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    }
}

/// Run a command and filter output to show only errors/warnings
pub fn run_err(command: &str, verbose: u8) -> Result<i32> {
    if verbose > 0 {
        eprintln!("Running: {}", command);
    }
    let cmd = build_shell_command(command);
    crate::core::runner::run_filtered(
        cmd,
        "err",
        command,
        filter_errors,
        crate::core::runner::RunOptions::with_tee("err")
            .postprocess(&[PostprocessKind::Stacktrace, PostprocessKind::BuildGroup])
            .failure_fallback(err_failure_fallback),
    )
}

/// Run tests and show only failures
pub fn run_test(command: &str, verbose: u8) -> Result<i32> {
    if verbose > 0 {
        eprintln!("Running tests: {}", command);
    }
    let cmd = build_shell_command(command);
    let command_owned = command.to_string();
    crate::core::runner::run_filtered(
        cmd,
        "test",
        command,
        move |raw| extract_test_summary(raw, &command_owned),
        crate::core::runner::RunOptions::with_tee("test"),
    )
}

fn filter_errors(output: &str) -> String {
    let mut result = Vec::new();
    let mut in_error_block = false;
    let mut blank_count = 0;

    for line in output.lines() {
        let is_error_line = ERROR_PATTERNS.iter().any(|p| p.is_match(line));

        if is_error_line {
            in_error_block = true;
            blank_count = 0;
            result.push(line.to_string());
        } else if in_error_block {
            if line.trim().is_empty() {
                blank_count += 1;
                if blank_count >= 2 {
                    in_error_block = false;
                } else {
                    result.push(line.to_string());
                }
            } else if line.starts_with(' ') || line.starts_with('\t') {
                result.push(line.to_string());
                blank_count = 0;
            } else {
                in_error_block = false;
            }
        }
    }

    if result.is_empty() {
        NO_ERRORS_WARNINGS_MATCHED.to_string()
    } else {
        result.join("\n")
    }
}

fn err_failure_fallback(filtered: &str, raw: &str, exit_code: i32) -> Option<String> {
    if exit_code == 0 {
        return None;
    }

    if filtered.trim() != NO_ERRORS_WARNINGS_MATCHED {
        return None;
    }

    let tail = raw_tail(raw, FAILURE_TAIL_LINES);
    let mut output = format!(
        "[rtk] command failed with exit code {exit_code}; no errors/warnings matched"
    );

    if tail.is_empty() {
        output.push_str("\n[rtk] command produced no output");
    } else {
        output.push_str(&format!(
            "\nRAW OUTPUT (last {} lines):",
            tail.lines().count()
        ));
        for line in tail.lines() {
            output.push_str(&format!("\n  {line}"));
        }
    }

    Some(output)
}

fn raw_tail(raw: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = raw.lines().filter(|line| !line.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn extract_test_summary(output: &str, command: &str) -> String {
    let mut result = Vec::new();
    let lines: Vec<&str> = output.lines().collect();

    let is_cargo = command.contains("cargo test");
    let is_pytest = command.contains("pytest");
    let is_jest =
        command.contains("jest") || command.contains("npm test") || command.contains("yarn test");
    let is_go = command.contains("go test");

    let mut failures = Vec::new();
    let mut in_failure = false;
    let mut failure_lines = Vec::new();

    for line in lines.iter() {
        if is_cargo {
            if line.contains("test result:") {
                result.push(line.to_string());
            }
            if line.contains("FAILED") && !line.contains("test result") {
                failures.push(line.to_string());
            }
            if line.starts_with("failures:") {
                in_failure = true;
            }
            if in_failure && line.starts_with("    ") {
                failure_lines.push(line.to_string());
            }
        }

        if is_pytest {
            if line.contains(" passed") || line.contains(" failed") || line.contains(" error") {
                result.push(line.to_string());
            }
            if line.contains("FAILED") {
                failures.push(line.to_string());
            }
        }

        if is_jest {
            if line.contains("Tests:") || line.contains("Test Suites:") {
                result.push(line.to_string());
            }
            if line.contains("✕") || line.contains("FAIL") {
                failures.push(line.to_string());
            }
        }

        if is_go {
            if line.starts_with("ok") || line.starts_with("FAIL") || line.starts_with("---") {
                result.push(line.to_string());
            }
            if line.contains("FAIL") {
                failures.push(line.to_string());
            }
        }
    }

    let mut output = String::new();

    if !failures.is_empty() {
        output.push_str("[FAIL] FAILURES:\n");
        for f in failures.iter().take(10) {
            output.push_str(&format!("  {}\n", f));
        }
        if failures.len() > 10 {
            output.push_str(&format!("  ... +{} more failures\n", failures.len() - 10));
        }
        for f in failure_lines.iter().take(20) {
            output.push_str(&format!("  {}\n", f.trim()));
        }
        if failure_lines.len() > 20 {
            output.push_str(&format!("  ... +{} more\n", failure_lines.len() - 20));
        }
        output.push('\n');
    }

    if !result.is_empty() {
        output.push_str("SUMMARY:\n");
        for r in &result {
            output.push_str(&format!("  {}\n", r));
        }
    } else {
        output.push_str("OUTPUT (last 5 lines):\n");
        let start = lines.len().saturating_sub(5);
        for line in &lines[start..] {
            if !line.trim().is_empty() {
                output.push_str(&format!("  {}\n", line));
            }
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_errors() {
        let output = "info: compiling\nerror: something failed\n  at line 10\ninfo: done";
        let filtered = filter_errors(output);
        assert!(filtered.contains("error"));
        assert!(!filtered.contains("info"));
    }

    #[test]
    fn filter_errors_reports_no_matches_for_clean_output() {
        let filtered = filter_errors("building\nfinished");

        assert_eq!(filtered, NO_ERRORS_WARNINGS_MATCHED);
    }

    #[test]
    fn err_failure_fallback_adds_exit_code_and_raw_tail() {
        let raw = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9";
        let fallback = err_failure_fallback(NO_ERRORS_WARNINGS_MATCHED, raw, 42).unwrap();

        assert!(fallback.contains("command failed with exit code 42"));
        assert!(fallback.contains("RAW OUTPUT (last 8 lines):"));
        assert!(!fallback.contains("  line 1"));
        assert!(fallback.contains("  line 2"));
        assert!(fallback.contains("  line 9"));
    }

    #[test]
    fn err_failure_fallback_preserves_real_diagnostics() {
        let fallback = err_failure_fallback("error: real failure", "raw output", 1);

        assert!(fallback.is_none());
    }
}
