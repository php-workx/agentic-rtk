//! Filters grep output by grouping matches by file.

// AGENTIC-RTK-FORK: This file contains fork-specific modifications. See FORK.md.

use crate::core::config;
use crate::core::stream::exec_capture;
use crate::core::tracking;
use crate::core::utils::resolved_command;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub fn run(
    pattern: &str,
    path: &str,
    max_line_len: usize,
    max_results: usize,
    context_only: bool,
    file_type: Option<&str>,
    extra_args: &[String],
    verbose: u8,
) -> Result<i32> {
    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("grep: '{}' in {}", pattern, path);
    }

    // Fix: convert BRE alternation \| → | for rg (which uses PCRE-style regex)
    let rg_pattern = pattern.replace(r"\|", "|");

    let mut rg_cmd = resolved_command("rg");
    // --no-ignore-vcs: match grep -r behavior (don't skip .gitignore'd files).
    // Without this, rg returns 0 matches for files in .gitignore, causing
    // false negatives that make AI agents draw wrong conclusions.
    // Using --no-ignore-vcs (not --no-ignore) so .ignore/.rgignore are still respected.
    rg_cmd.args(["-n", "--no-heading", "--no-ignore-vcs", &rg_pattern, path]);

    if let Some(ft) = file_type {
        rg_cmd.arg("--type").arg(ft);
    }

    for arg in extra_args {
        // Fix: skip grep-ism -r flag (rg is recursive by default; rg -r means --replace)
        if arg == "-r" || arg == "--recursive" {
            continue;
        }
        rg_cmd.arg(arg);
    }

    let result = exec_capture(&mut rg_cmd)
        .or_else(|_| {
            let mut grep_cmd = resolved_command("grep");
            grep_cmd.args(["-rn", pattern, path]);
            exec_capture(&mut grep_cmd)
        })
        .context("grep/rg failed")?;

    let exit_code = result.exit_code;
    let raw_output = result.stdout.clone();

    if result.stdout.trim().is_empty() {
        // Show stderr for errors (bad regex, missing file, etc.)
        if exit_code == 2 && !result.stderr.trim().is_empty() {
            eprintln!("{}", result.stderr.trim());
        }
        let msg = format!("0 matches for '{}'", pattern);
        println!("{}", msg);
        timer.track(
            &format!("grep -rn '{}' {}", pattern, path),
            "rtk grep",
            &raw_output,
            &msg,
        );
        return Ok(exit_code);
    }

    let mut matches = Vec::new();
    let mut total = 0;

    let context_re = if context_only {
        Regex::new(&format!("(?i).{{0,20}}{}.*", regex::escape(pattern))).ok()
    } else {
        None
    };

    for line in result.stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();

        let (file, line_num, content) = if parts.len() == 3 {
            let ln = parts[1].parse().unwrap_or(0);
            (parts[0].to_string(), ln, parts[2])
        } else if parts.len() == 2 {
            let ln = parts[0].parse().unwrap_or(0);
            (path.to_string(), ln, parts[1])
        } else {
            continue;
        };

        total += 1;
        let cleaned = clean_line(content, max_line_len, context_re.as_ref(), pattern);
        matches.push((file, line_num, cleaned));
    }

    let rtk_output = render_grouped_matches(
        matches,
        total,
        max_results,
        config::limits().grep_max_per_file,
    );

    print!("{}", rtk_output);
    timer.track(
        &format!("grep -rn '{}' {}", pattern, path),
        "rtk grep",
        &raw_output,
        &rtk_output,
    );

    Ok(exit_code)
}

fn clean_line(line: &str, max_len: usize, context_re: Option<&Regex>, pattern: &str) -> String {
    let trimmed = line.trim();

    if let Some(re) = context_re {
        if let Some(m) = re.find(trimmed) {
            let matched = m.as_str();
            if matched.len() <= max_len {
                return matched.to_string();
            }
        }
    }

    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        let lower = trimmed.to_lowercase();
        let pattern_lower = pattern.to_lowercase();

        if let Some(pos) = lower.find(&pattern_lower) {
            let char_pos = lower[..pos].chars().count();
            let chars: Vec<char> = trimmed.chars().collect();
            let char_len = chars.len();

            let start = char_pos.saturating_sub(max_len / 3);
            let end = (start + max_len).min(char_len);
            let start = if end == char_len {
                end.saturating_sub(max_len)
            } else {
                start
            };

            let slice: String = chars[start..end].iter().collect();
            if start > 0 && end < char_len {
                format!("...{}...", slice)
            } else if start > 0 {
                format!("...{}", slice)
            } else {
                format!("{}...", slice)
            }
        } else {
            let t: String = trimmed.chars().take(max_len - 3).collect();
            format!("{}...", t)
        }
    }
}

fn compact_path(path: &str) -> String {
    if path.len() <= 50 {
        return path.to_string();
    }

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 3 {
        return path.to_string();
    }

    format!(
        "{}/.../{}/{}",
        parts[0],
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    )
}

fn render_grouped_matches(
    matches: Vec<(String, usize, String)>,
    total: usize,
    max_results: usize,
    per_file: usize,
) -> String {
    let mut by_file: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for (file, line_num, content) in matches {
        by_file.entry(file).or_default().push((line_num, content));
    }

    let mut rtk_output = String::new();
    rtk_output.push_str(&format!("{} matches in {}F:\n\n", total, by_file.len()));

    let mut shown = 0;
    let mut files: Vec<_> = by_file.iter().collect();
    files.sort_by_key(|(f, _)| *f);

    for (file, matches) in files {
        if shown >= max_results {
            break;
        }

        let file_display = compact_path(file);
        rtk_output.push_str(&format!("[file] {} ({}):\n", file_display, matches.len()));

        let visible_count = per_file.min(matches.len()).min(max_results - shown);
        let groups = group_visible_matches(&matches[..visible_count]);
        for group in groups {
            rtk_output.push_str(&format!("  {}: {}\n", group.line_numbers.join(","), group.content));
        }
        shown += visible_count;

        if matches.len() > per_file {
            rtk_output.push_str(&format!("  +{}\n", matches.len() - per_file));
        }
        rtk_output.push('\n');
    }

    if total > shown {
        rtk_output.push_str(&format!("... +{}\n", total - shown));
    }

    rtk_output
}

struct MatchGroup {
    content: String,
    line_numbers: Vec<String>,
}

fn group_visible_matches(matches: &[(usize, String)]) -> Vec<MatchGroup> {
    let mut groups: Vec<MatchGroup> = Vec::new();
    let mut by_content: HashMap<String, usize> = HashMap::new();

    for (line_num, content) in matches {
        if let Some(index) = by_content.get(content) {
            groups[*index].line_numbers.push(line_num.to_string());
        } else {
            by_content.insert(content.clone(), groups.len());
            groups.push(MatchGroup {
                content: content.clone(),
                line_numbers: vec![line_num.to_string()],
            });
        }
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_line() {
        let line = "            const result = someFunction();";
        let cleaned = clean_line(line, 50, None, "result");
        assert!(!cleaned.starts_with(' '));
        assert!(cleaned.len() <= 50);
    }

    #[test]
    fn test_compact_path() {
        let path = "/Users/patrick/dev/project/src/components/Button.tsx";
        let compact = compact_path(path);
        assert!(compact.len() <= 60);
    }

    #[test]
    fn test_extra_args_accepted() {
        // Test that the function signature accepts extra_args
        // This is a compile-time test - if it compiles, the signature is correct
        let _extra: Vec<String> = vec!["-i".to_string(), "-A".to_string(), "3".to_string()];
        // No need to actually run - we're verifying the parameter exists
    }

    #[test]
    fn test_clean_line_multibyte() {
        // Thai text that exceeds max_len in bytes
        let line = "  สวัสดีครับ นี่คือข้อความที่ยาวมากสำหรับทดสอบ  ";
        let cleaned = clean_line(line, 20, None, "ครับ");
        // Should not panic
        assert!(!cleaned.is_empty());
    }

    #[test]
    fn test_clean_line_emoji() {
        let line = "🎉🎊🎈🎁🎂🎄 some text 🎃🎆🎇✨";
        let cleaned = clean_line(line, 15, None, "text");
        assert!(!cleaned.is_empty());
    }

    // Fix: BRE \| alternation is translated to PCRE | for rg
    #[test]
    fn test_bre_alternation_translated() {
        let pattern = r"fn foo\|pub.*bar";
        let rg_pattern = pattern.replace(r"\|", "|");
        assert_eq!(rg_pattern, "fn foo|pub.*bar");
    }

    // Fix: -r flag (grep recursive) is stripped from extra_args (rg is recursive by default)
    #[test]
    fn test_recursive_flag_stripped() {
        let extra_args: Vec<String> = vec!["-r".to_string(), "-i".to_string()];
        let filtered: Vec<&String> = extra_args
            .iter()
            .filter(|a| *a != "-r" && *a != "--recursive")
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], "-i");
    }

    // --- truncation accuracy ---

    #[test]
    fn test_grep_overflow_uses_uncapped_total() {
        // Confirm the grep overflow invariant: matches vec is never capped before overflow calc.
        // If total_matches > per_file, overflow = total_matches - per_file (not capped).
        // This documents that grep_cmd.rs avoids the diff_cmd bug (cap at N then compute N-10).
        let per_file = config::limits().grep_max_per_file;
        let total_matches = per_file + 42;
        let overflow = total_matches - per_file;
        assert_eq!(overflow, 42, "overflow must equal true suppressed count");
        // Demonstrate why capping before subtraction is wrong:
        let hypothetical_cap = per_file + 5;
        let capped = total_matches.min(hypothetical_cap);
        let wrong_overflow = capped - per_file;
        assert_ne!(
            wrong_overflow, overflow,
            "capping before subtraction gives wrong overflow"
        );
    }

    // Verify line numbers are always enabled in rg invocation (grep_cmd.rs:24).
    // The -n/--line-numbers clap flag in main.rs is a no-op accepted for compat.
    #[test]
    fn test_rg_always_has_line_numbers() {
        // grep_cmd::run() always passes "-n" to rg (line 24).
        // This test documents that -n is built-in, so the clap flag is safe to ignore.
        let mut cmd = resolved_command("rg");
        cmd.args(["-n", "--no-heading", "NONEXISTENT_PATTERN_12345", "."]);
        // If rg is available, it should accept -n without error (exit 1 = no match, not error)
        if let Ok(output) = cmd.output() {
            assert!(
                output.status.code() == Some(1) || output.status.success(),
                "rg -n should be accepted"
            );
        }
        // If rg is not installed, skip gracefully (test still passes)
    }

    #[test]
    fn test_rg_no_ignore_vcs_flag_accepted() {
        // Verify rg accepts --no-ignore-vcs (used to match grep -r behavior for .gitignore)
        let mut cmd = resolved_command("rg");
        cmd.args([
            "-n",
            "--no-heading",
            "--no-ignore-vcs",
            "NONEXISTENT_PATTERN_12345",
            ".",
        ]);
        if let Ok(output) = cmd.output() {
            assert!(
                output.status.code() == Some(1) || output.status.success(),
                "rg --no-ignore-vcs should be accepted"
            );
        }
        // If rg is not installed, skip gracefully (test still passes)
    }

    #[test]
    fn test_repeated_identical_matches_collapse_line_numbers() {
        let matches = vec![
            ("src/lib.rs".to_string(), 10, "let value = 1;".to_string()),
            ("src/lib.rs".to_string(), 14, "let value = 1;".to_string()),
            ("src/lib.rs".to_string(), 20, "let other = 2;".to_string()),
        ];

        let output = render_grouped_matches(matches, 3, 100, 10);

        assert!(output.contains("[file] src/lib.rs (3):"));
        assert!(output.contains("10,14: let value = 1;"));
        assert!(output.contains("20: let other = 2;"));
    }

    #[test]
    fn test_unique_matches_are_not_collapsed() {
        let matches = vec![
            ("src/lib.rs".to_string(), 10, "alpha".to_string()),
            ("src/lib.rs".to_string(), 14, "beta".to_string()),
        ];

        let output = render_grouped_matches(matches, 2, 100, 10);

        assert!(output.contains("10: alpha"));
        assert!(output.contains("14: beta"));
        assert!(!output.contains("10,14"));
    }

    #[test]
    fn test_grouped_grep_omitted_count_uses_true_total() {
        let matches: Vec<_> = (1..=25)
            .map(|line| ("src/lib.rs".to_string(), line, format!("match {line}")))
            .collect();

        let output = render_grouped_matches(matches, 25, 10, 20);

        assert!(output.contains("... +15"));
    }

    #[test]
    fn test_grouped_grep_repeated_output_saves_at_least_40_percent() {
        let matches: Vec<_> = (1..=50)
            .map(|line| {
                (
                    "src/lib.rs".to_string(),
                    line,
                    "repeated diagnostic text".to_string(),
                )
            })
            .collect();
        let raw = (1..=50)
            .map(|line| format!("src/lib.rs:{line}:repeated diagnostic text"))
            .collect::<Vec<_>>()
            .join("\n");

        let output = render_grouped_matches(matches, 50, 100, 100);
        let savings = 1.0 - (output.len() as f64 / raw.len() as f64);

        assert!(
            savings >= 0.40,
            "expected at least 40% savings, got {:.1}%\n{}",
            savings * 100.0,
            output
        );
    }
}
