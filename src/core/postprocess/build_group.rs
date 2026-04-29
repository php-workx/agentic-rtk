//! Conservative grouping for generic build diagnostics.

use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;

lazy_static! {
    static ref TSC_ERROR: Regex =
        Regex::new(r"^(.+?)\((\d+),\d+\):\s+(?:error|warning)\s+(TS\d+):\s+(.+)$").unwrap();
    static ref CARGO_ERROR_HEADER: Regex = Regex::new(r"^error\[(E\d{4})\]:\s+(.+)$").unwrap();
    static ref CARGO_LOCATION: Regex = Regex::new(r"^\s+--> (.+?):(\d+):\d+$").unwrap();
    static ref MYPY_ERROR: Regex =
        Regex::new(r"^(.+?):(\d+)(?::\d+)?: (?:error|warning): (.+?)(?:\s+\[(.+)\])?$").unwrap();
    static ref PYLINT_ERROR: Regex =
        Regex::new(r"^(.+?):(\d+):\d+: ([CWER]\d{4}): (.+?) \((.+)\)$").unwrap();
}

#[derive(Debug)]
struct ErrorGroup {
    code: String,
    message: String,
    locations: HashMap<String, Vec<usize>>,
    count: usize,
}

impl ErrorGroup {
    fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
            locations: HashMap::new(),
            count: 0,
        }
    }

    fn add_location(&mut self, file: &str, line: usize) {
        self.locations
            .entry(file.to_string())
            .or_default()
            .push(line);
        self.count += 1;
    }
}

pub fn group_build_errors(input: &str) -> String {
    let mut groups: HashMap<String, ErrorGroup> = HashMap::new();
    let lines: Vec<&str> = input.lines().collect();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];

        if let Some(caps) = TSC_ERROR.captures(line) {
            add_group(
                &mut groups,
                &caps[3],
                &caps[4],
                &caps[1],
                parse_usize(&caps[2]),
            );
            index += 1;
            continue;
        }

        if let Some(caps) = CARGO_ERROR_HEADER.captures(line) {
            let code = caps[1].to_string();
            let message = caps[2].to_string();
            for candidate in lines
                .iter()
                .take((index + 6).min(lines.len()))
                .skip(index + 1)
            {
                if let Some(loc_caps) = CARGO_LOCATION.captures(candidate) {
                    add_group(
                        &mut groups,
                        &code,
                        &message,
                        &loc_caps[1],
                        parse_usize(&loc_caps[2]),
                    );
                    break;
                }
            }
            index += 1;
            continue;
        }

        if let Some(caps) = MYPY_ERROR.captures(line) {
            let code = caps.get(4).map(|m| m.as_str()).unwrap_or("mypy");
            add_group(&mut groups, code, &caps[3], &caps[1], parse_usize(&caps[2]));
            index += 1;
            continue;
        }

        if let Some(caps) = PYLINT_ERROR.captures(line) {
            add_group(
                &mut groups,
                &caps[3],
                &caps[4],
                &caps[1],
                parse_usize(&caps[2]),
            );
            index += 1;
            continue;
        }

        index += 1;
    }

    let has_repeated_group = groups.values().any(|group| group.count > 1);
    if groups.is_empty() || (groups.len() == 1 && !has_repeated_group) {
        return input.to_string();
    }

    let mut sorted: Vec<&ErrorGroup> = groups.values().collect();
    sorted.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.code.cmp(&b.code)));

    let mut result = String::new();
    for group in sorted {
        if group.count > 1 {
            result.push_str(&format!(
                "{}: {} (x{})\n",
                group.code, group.message, group.count
            ));
        } else {
            result.push_str(&format!("{}: {}\n", group.code, group.message));
        }

        let mut file_entries: Vec<(&String, &Vec<usize>)> = group.locations.iter().collect();
        file_entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
        let omitted_entries = file_entries.len().saturating_sub(8);
        let omitted_locations = file_entries
            .iter()
            .skip(8)
            .map(|(_, line_nums)| unique_line_count(line_nums))
            .sum::<usize>();

        for (file, line_nums) in file_entries.iter().take(8) {
            let mut sorted_lines = (*line_nums).clone();
            sorted_lines.sort_unstable();
            sorted_lines.dedup();
            let lines = sorted_lines
                .iter()
                .map(|line| format!(":{}", line))
                .collect::<Vec<_>>()
                .join(", ");
            result.push_str(&format!("  {}  {}\n", file, lines));
        }

        if omitted_entries > 0 {
            result.push_str(&format!(
                "  ... +{} more files ({} locations)\n",
                omitted_entries, omitted_locations
            ));
        }
    }

    let grouped = result.trim_end().to_string();
    if has_repeated_group || grouped.len() < input.len() {
        grouped
    } else {
        input.to_string()
    }
}

fn add_group(
    groups: &mut HashMap<String, ErrorGroup>,
    code: &str,
    message: &str,
    file: &str,
    line: usize,
) {
    let group = groups
        .entry(code.to_string())
        .or_insert_with(|| ErrorGroup::new(code, message));
    group.add_location(file, line);
}

fn parse_usize(value: &str) -> usize {
    value.parse().unwrap_or(0)
}

fn unique_line_count(line_nums: &[usize]) -> usize {
    let mut sorted_lines = line_nums.to_vec();
    sorted_lines.sort_unstable();
    sorted_lines.dedup();
    sorted_lines.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_repeated_typescript_errors() {
        let input = r#"src/a.ts(10,5): error TS2322: Type 'string' is not assignable to type 'number'.
src/b.ts(12,7): error TS2322: Type 'string' is not assignable to type 'number'.
src/c.ts(2,1): error TS7006: Parameter 'x' implicitly has an 'any' type."#;

        let result = group_build_errors(input);
        assert!(result.contains("TS2322"));
        assert!(result.contains("x2"));
        assert!(result.contains("src/a.ts"));
        assert!(result.contains("src/b.ts"));
    }

    #[test]
    fn leaves_unrecognized_output_unchanged() {
        let input = "building project\nall good\n";
        assert_eq!(group_build_errors(input), input);
    }

    #[test]
    fn groups_single_repeated_diagnostic_code() {
        let input = r#"src/a.ts(10,5): error TS2322: Type 'string' is not assignable to type 'number'.
src/b.ts(12,7): error TS2322: Type 'string' is not assignable to type 'number'.
src/c.ts(14,9): error TS2322: Type 'string' is not assignable to type 'number'."#;

        let result = group_build_errors(input);

        assert_ne!(result, input);
        assert!(result.contains("TS2322"));
        assert!(result.contains("x3"));
        assert!(result.contains("src/a.ts"));
        assert!(result.contains("src/b.ts"));
        assert!(result.contains("src/c.ts"));
    }

    #[test]
    fn summarizes_omitted_file_entries() {
        let input = r#"src/a.ts(1,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/b.ts(2,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/c.ts(3,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/d.ts(4,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/e.ts(5,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/f.ts(6,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/g.ts(7,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/h.ts(8,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/i.ts(9,1): error TS2322: Type 'string' is not assignable to type 'number'.
src/j.ts(10,1): error TS7006: Parameter 'x' implicitly has an 'any' type."#;

        let result = group_build_errors(input);

        assert!(result.contains("TS2322"));
        assert!(result.contains("... +1 more files"));
    }
}
