//! Deterministic text noise cleanup shared by live and historical filters.

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref ANSI_RE: Regex = Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap();
    static ref PROGRESS_BAR_RE: Regex = Regex::new(r"[█░▓▒#=]{5,}.*\d{1,3}%|[█░▓▒]{5,}").unwrap();
    static ref PERCENT_RE: Regex = Regex::new(r"(\d{1,3})%").unwrap();
    static ref DECORATION_RE: Regex = Regex::new(r"^[\s]*([═─━\-\*=~]{5,})[\s]*$").unwrap();
    static ref SPINNER_RE: Regex = Regex::new(r"^[\s]*[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏⣾⣽⣻⢿⡿⣟⣯⣷]").unwrap();
    static ref IMPORTANT_RE: Regex =
        Regex::new(r"(?i)\b(error|warn(ing)?|fail(ed|ure)?|panic|exception)\b").unwrap();
    static ref TIMESTAMP_RE: Regex =
        Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}|\d{2}:\d{2}:\d{2}|\[[\d:T\-]+\]").unwrap();
}

pub fn clean_text_noise(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let cr_resolved = resolve_carriage_returns(input);
    let ansi_stripped = ANSI_RE.replace_all(&cr_resolved, "").to_string();
    filter_noise_lines(&ansi_stripped)
}

fn resolve_carriage_returns(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            if line.contains('\r') {
                line.rsplit('\r').find(|s| !s.is_empty()).unwrap_or("")
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn filter_noise_lines(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if IMPORTANT_RE.is_match(trimmed) || TIMESTAMP_RE.is_match(trimmed) {
            result.push(line);
            i += 1;
            continue;
        }

        if SPINNER_RE.is_match(trimmed) {
            i += 1;
            continue;
        }

        if is_progress_line(trimmed) {
            let mut last_progress_idx = i;
            let mut j = i + 1;
            while j < lines.len() {
                let next = lines[j].trim();
                if is_progress_line(next) || SPINNER_RE.is_match(next) {
                    if is_progress_line(next) {
                        last_progress_idx = j;
                    }
                    j += 1;
                } else {
                    break;
                }
            }
            if extract_percent(lines[last_progress_idx].trim()) == Some(100) {
                result.push(lines[last_progress_idx]);
            }
            i = j;
            continue;
        }

        if is_decoration_line(trimmed) {
            i += 1;
            continue;
        }

        result.push(line);
        i += 1;
    }

    result.join("\n")
}

fn is_progress_line(line: &str) -> bool {
    PROGRESS_BAR_RE.is_match(line)
}

fn extract_percent(line: &str) -> Option<u32> {
    PERCENT_RE
        .captures(line)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn is_decoration_line(line: &str) -> bool {
    if DECORATION_RE.is_match(line) {
        return true;
    }

    let trimmed = line.trim();
    if trimmed.len() < 5 {
        return false;
    }

    let mut chars = trimmed.chars();
    if let Some(first) = chars.next() {
        if matches!(first, '═' | '─' | '━' | '-' | '*' | '=' | '~') {
            return chars.all(|c| c == first || c.is_whitespace());
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_and_spinner_noise() {
        let input = "\x1b[31merror\x1b[0m\n⠋ loading\nactual line";
        assert_eq!(clean_text_noise(input), "error\nactual line");
    }

    #[test]
    fn keeps_final_progress_only() {
        let input = "████░░ 40%\n██████████ 100%\ndone";
        assert_eq!(clean_text_noise(input), "██████████ 100%\ndone");
    }

    #[test]
    fn preserves_important_progress_line() {
        let input = "warning ████░░ 40%\n-----\nok";
        assert_eq!(clean_text_noise(input), "warning ████░░ 40%\nok");
    }
}
