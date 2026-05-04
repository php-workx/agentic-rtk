//! Stacktrace compression for common runtimes.

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref NODE_FRAME_RE: Regex = Regex::new(r"^\s+at\s+.+\(.+:\d+:\d+\)").unwrap();
    static ref NODE_FRAME_BARE_RE: Regex = Regex::new(r"^\s+at\s+.+:\d+:\d+").unwrap();
    static ref NODE_EXTRACT_RE: Regex =
        Regex::new(r"^\s+at\s+(?:(.+?)\s+\()?(.+):(\d+):\d+\)?").unwrap();
    static ref PYTHON_TRACEBACK_RE: Regex =
        Regex::new(r"^Traceback \(most recent call last\)").unwrap();
    static ref PYTHON_FILE_RE: Regex = Regex::new(r#"^\s+File "(.+)", line (\d+)"#).unwrap();
    static ref RUST_PANIC_RE: Regex = Regex::new(r"^thread '.*' panicked at").unwrap();
    static ref RUST_BACKTRACE_FRAME_RE: Regex = Regex::new(r"^\s+\d+:\s+").unwrap();
    static ref RUST_FRAME_EXTRACT_RE: Regex = Regex::new(r"^\s+\d+:\s+(.+)").unwrap();
    static ref RUST_BACKTRACE_AT_RE: Regex = Regex::new(r"^\s+at\s+(.+)").unwrap();
    static ref RUST_LOCATION_RE: Regex = Regex::new(r"^\s+at\s+(.+):(\d+):\d+").unwrap();
    static ref GO_GOROUTINE_RE: Regex = Regex::new(r"^goroutine \d+").unwrap();
    static ref GO_FUNC_RE: Regex = Regex::new(r"^[\w./]+\(").unwrap();
    static ref GO_FRAME_RE: Regex = Regex::new(r"^\s+.+\.go:\d+").unwrap();
    static ref JAVA_EXTRACT_RE: Regex = Regex::new(r"^\s+at\s+([\w.$]+)\(([\w.]+):(\d+)\)").unwrap();
    static ref NODE_FRAMEWORK_RE: Regex = Regex::new(r"node_modules/|node:internal/").unwrap();
    static ref PYTHON_FRAMEWORK_RE: Regex =
        Regex::new(r"site-packages/|/usr/lib/python|importlib|_bootstrap").unwrap();
    static ref RUST_FRAMEWORK_RE: Regex = Regex::new(
        r"std::rt::|tokio::runtime::|std::panicking::|std::sys::|core::panicking::|core::ops::function::|__rust_begin_short_backtrace|__rust_end_short_backtrace"
    ).unwrap();
    static ref JAVA_FRAMEWORK_RE: Regex = Regex::new(
        r"java\.lang\.reflect\.|sun\.reflect\.|org\.springframework\.|java\.util\.concurrent\.|jdk\.internal\.|java\.net\.|sun\.net\.|org\.apache\."
    ).unwrap();
    static ref GO_FRAMEWORK_RE: Regex = Regex::new(r"^\s*(runtime[./]|net/http\.)").unwrap();
}

#[derive(Debug, PartialEq, Eq)]
enum Language {
    NodeJs,
    Python,
    Rust,
    Go,
    Java,
}

pub fn compress_errors(input: &str) -> String {
    let deduped = deduplicate_repeated_lines(input);
    let compressed = match detect_language(&deduped) {
        Some(Language::NodeJs) => compress_nodejs(&deduped),
        Some(Language::Python) => compress_python(&deduped),
        Some(Language::Rust) => compress_rust(&deduped),
        Some(Language::Go) => compress_go(&deduped),
        Some(Language::Java) => compress_java(&deduped),
        None => deduped,
    };

    if compressed.len() <= input.len() {
        compressed
    } else {
        input.to_string()
    }
}

fn detect_language(input: &str) -> Option<Language> {
    for line in input.lines() {
        if PYTHON_TRACEBACK_RE.is_match(line) || PYTHON_FILE_RE.is_match(line) {
            return Some(Language::Python);
        }
        if RUST_PANIC_RE.is_match(line) {
            return Some(Language::Rust);
        }
        if GO_GOROUTINE_RE.is_match(line) {
            return Some(Language::Go);
        }
    }

    for line in input.lines() {
        if let Some(caps) = JAVA_EXTRACT_RE.captures(line) {
            let method = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if method.contains('.') && !method.contains('/') {
                return Some(Language::Java);
            }
        }
        if NODE_FRAME_RE.is_match(line) || NODE_FRAME_BARE_RE.is_match(line) {
            return Some(Language::NodeJs);
        }
    }

    None
}

fn deduplicate_repeated_lines(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut result = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let current = lines[index];
        let mut count = 1;
        while index + count < lines.len() && lines[index + count] == current {
            count += 1;
        }
        result.push(current.to_string());
        if count > 1 {
            result.push(format!("  (repeated {} times)", count));
        }
        index += count;
    }

    if result.is_empty() {
        input.to_string()
    } else {
        result.join("\n")
    }
}

fn flush_hidden(result: &mut Vec<String>, hidden_count: &mut usize) {
    if *hidden_count > 0 {
        result.push(format!("  (+ {} framework frames hidden)", hidden_count));
        *hidden_count = 0;
    }
}

fn compress_nodejs(input: &str) -> String {
    let mut result = Vec::new();
    let mut hidden_count = 0;

    for line in input.lines() {
        let is_frame = NODE_FRAME_RE.is_match(line) || NODE_FRAME_BARE_RE.is_match(line);
        if is_frame && NODE_FRAMEWORK_RE.is_match(line) {
            hidden_count += 1;
            continue;
        }
        if is_frame {
            flush_hidden(&mut result, &mut hidden_count);
            if let Some(caps) = NODE_EXTRACT_RE.captures(line) {
                let func = caps.get(1).map(|m| m.as_str()).unwrap_or("<anonymous>");
                let file = caps.get(2).map(|m| m.as_str()).unwrap_or("?");
                let line_num = caps.get(3).map(|m| m.as_str()).unwrap_or("?");
                result.push(format!("  -> {}:{} {}", file.trim(), line_num, func.trim()));
            } else {
                result.push(format!("  -> {}", line.trim()));
            }
        } else {
            flush_hidden(&mut result, &mut hidden_count);
            result.push(line.to_string());
        }
    }

    flush_hidden(&mut result, &mut hidden_count);
    result.join("\n")
}

fn compress_python(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut result = Vec::new();
    let mut hidden_count = 0;
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        if let Some(caps) = PYTHON_FILE_RE.captures(line) {
            let file = caps.get(1).map(|m| m.as_str()).unwrap_or("?");
            let line_num = caps.get(2).map(|m| m.as_str()).unwrap_or("?");
            let code = if index + 1 < lines.len()
                && !PYTHON_FILE_RE.is_match(lines[index + 1])
                && !lines[index + 1].starts_with("Traceback")
            {
                index += 1;
                Some(lines[index].trim())
            } else {
                None
            };

            if PYTHON_FRAMEWORK_RE.is_match(file) {
                hidden_count += 1;
            } else {
                flush_hidden(&mut result, &mut hidden_count);
                match code {
                    Some(code) if !code.is_empty() => {
                        result.push(format!("  -> {}:{} {}", file, line_num, code));
                    }
                    _ => result.push(format!("  -> {}:{}", file, line_num)),
                }
            }
        } else {
            flush_hidden(&mut result, &mut hidden_count);
            result.push(line.to_string());
        }
        index += 1;
    }

    flush_hidden(&mut result, &mut hidden_count);
    result.join("\n")
}

fn is_rust_framework_line(line: &str) -> bool {
    RUST_FRAMEWORK_RE.is_match(line)
        || line.contains("/rustc/")
        || line.contains(".cargo/registry/")
}

fn compress_rust(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut result = Vec::new();
    let mut hidden_count = 0;
    let mut index = 0;
    let mut skip_next_at = false;

    while index < lines.len() {
        let line = lines[index];
        if RUST_BACKTRACE_FRAME_RE.is_match(line) {
            let func_name = RUST_FRAME_EXTRACT_RE
                .captures(line)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim())
                .unwrap_or("");
            let at_line =
                if index + 1 < lines.len() && RUST_BACKTRACE_AT_RE.is_match(lines[index + 1]) {
                    Some(lines[index + 1])
                } else {
                    None
                };

            if is_rust_framework_line(line) || at_line.is_some_and(is_rust_framework_line) {
                hidden_count += 1;
                skip_next_at = at_line.is_some();
            } else {
                flush_hidden(&mut result, &mut hidden_count);
                if let Some(loc) = at_line.and_then(|l| {
                    RUST_LOCATION_RE.captures(l).map(|c| {
                        let file = c.get(1).map(|m| m.as_str()).unwrap_or("?");
                        let line_num = c.get(2).map(|m| m.as_str()).unwrap_or("?");
                        format!("{}:{}", file, line_num)
                    })
                }) {
                    result.push(format!("  -> {} {}", loc, func_name));
                    skip_next_at = true;
                } else {
                    result.push(format!("  -> {}", func_name));
                }
            }
        } else if RUST_BACKTRACE_AT_RE.is_match(line) {
            if skip_next_at {
                skip_next_at = false;
            } else if is_rust_framework_line(line) {
                hidden_count += 1;
            } else {
                result.push(line.to_string());
            }
        } else {
            flush_hidden(&mut result, &mut hidden_count);
            skip_next_at = false;
            result.push(line.to_string());
        }
        index += 1;
    }

    flush_hidden(&mut result, &mut hidden_count);
    result.join("\n")
}

fn compress_go(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut result = Vec::new();
    let mut hidden_count = 0;
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        if GO_FUNC_RE.is_match(line.trim()) {
            if GO_FRAMEWORK_RE.is_match(line.trim()) {
                hidden_count += 1;
                index += 1;
                if index < lines.len() && GO_FRAME_RE.is_match(lines[index]) {
                    index += 1;
                }
                continue;
            }
            flush_hidden(&mut result, &mut hidden_count);
            result.push(format!("  -> {}", line.trim()));
            index += 1;
            if index < lines.len() && GO_FRAME_RE.is_match(lines[index]) {
                result.push(format!("  -> {}", lines[index].trim()));
                index += 1;
            }
            continue;
        }

        flush_hidden(&mut result, &mut hidden_count);
        result.push(line.to_string());
        index += 1;
    }

    flush_hidden(&mut result, &mut hidden_count);
    result.join("\n")
}

fn compress_java(input: &str) -> String {
    let mut result = Vec::new();
    let mut hidden_count = 0;

    for line in input.lines() {
        if JAVA_EXTRACT_RE.is_match(line) && JAVA_FRAMEWORK_RE.is_match(line) {
            hidden_count += 1;
            continue;
        }
        flush_hidden(&mut result, &mut hidden_count);
        result.push(line.to_string());
    }

    flush_hidden(&mut result, &mut hidden_count);
    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_stacktrace_hides_framework_frames() {
        let input = r#"TypeError: Cannot read properties of undefined
    at getUserProfile (/app/src/api/users.ts:47:12)
    at Router.handle (/app/node_modules/express/lib/router/index.js:45:12)
    at Module._compile (node:internal/modules/cjs/loader:1376:14)"#;

        let result = compress_errors(input);
        assert!(result.contains("TypeError"));
        assert!(result.contains("src/api/users.ts:47"));
        assert!(!result.contains("node_modules"));
        assert!(!result.contains("node:internal"));
        assert!(result.contains("framework frames hidden"));
    }

    #[test]
    fn python_traceback_hides_framework_frames() {
        let input = r#"Traceback (most recent call last):
  File "/app/src/handler.py", line 42, in process_request
    result = compute(data)
  File "/app/venv/lib/python3.11/site-packages/flask/app.py", line 1498, in __call__
    return self.wsgi_app(environ, start_response)
  File "/app/src/utils.py", line 18, in compute
    return x / y
ZeroDivisionError: division by zero"#;

        let result = compress_errors(input);
        assert!(result.contains("ZeroDivisionError"));
        assert!(result.contains("src/handler.py:42"));
        assert!(result.contains("src/utils.py:18"));
        assert!(!result.contains("site-packages"));
        assert!(result.contains("framework frames hidden"));
    }

    #[test]
    fn rust_panic_hides_runtime_frames() {
        let input = r#"thread 'main' panicked at 'index out of bounds', src/main.rs:42:10
stack backtrace:
   0: std::panicking::begin_panic_handler
   1: core::panicking::panic_fmt
   2: myapp::process_data
   3: myapp::main
   4: std::rt::lang_start"#;

        let result = compress_errors(input);
        assert!(result.contains("panicked at"));
        assert!(result.contains("myapp::process_data"));
        assert!(!result.contains("std::panicking::begin_panic_handler"));
        assert!(result.contains("framework frames hidden"));
    }

    #[test]
    fn go_panic_hides_runtime_frames() {
        let input = r#"panic: boom

goroutine 1 [running]:
runtime.gopanic()
        /usr/local/go/src/runtime/panic.go:884 +0x212
github.com/acme/app.Handler()
        /app/handler.go:12 +0x33"#;

        let result = compress_errors(input);
        assert!(result.contains("panic: boom"));
        assert!(result.contains("github.com/acme/app.Handler"));
        assert!(result.contains("/app/handler.go:12"));
        assert!(!result.contains("runtime.gopanic"));
    }

    #[test]
    fn java_stacktrace_hides_framework_frames() {
        let input = r#"java.lang.IllegalStateException: bad state
	at com.acme.App.run(App.java:42)
	at org.springframework.boot.SpringApplication.callRunner(SpringApplication.java:800)
	at java.util.concurrent.ThreadPoolExecutor.runWorker(ThreadPoolExecutor.java:1136)"#;

        let result = compress_errors(input);
        assert!(result.contains("bad state"));
        assert!(result.contains("com.acme.App.run"));
        assert!(!result.contains("SpringApplication"));
        assert!(!result.contains("ThreadPoolExecutor"));
    }
}
