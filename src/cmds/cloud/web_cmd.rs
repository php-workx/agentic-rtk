//! Fetches HTML pages and extracts readable content.

use crate::core::postprocess::web_extract;
use crate::core::tracking;
use anyhow::{Context, Result};
use std::borrow::Cow;
use std::io::Read;

pub fn run(url: &str, verbose: u8) -> Result<i32> {
    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("Fetching: {}", url);
    }

    let response = match ureq::get(url)
        .set("User-Agent", "rtk")
        .timeout(std::time::Duration::from_secs(30))
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(err) => return Err(err).with_context(|| format!("Failed to fetch {}", url)),
    };

    let status = response.status();
    let status_text = response.status_text().to_string();
    let content_type = response.header("content-type").unwrap_or("").to_string();
    const MAX_BODY_BYTES: u64 = 10_000_000; // 10 MiB
    let mut body = Vec::new();
    std::io::Read::take(response.into_reader(), MAX_BODY_BYTES)
        .read_to_end(&mut body)
        .with_context(|| format!("Failed to read response body from {}", url))?;

    let filtered = render_response(status, &status_text, &content_type, &body);

    println!("{}", filtered);
    let raw = String::from_utf8_lossy(&body);
    timer.track_with_feature(
        &format!("web {}", url),
        &format!("rtk web {}", url),
        &raw,
        &filtered,
        "web-extract",
    );

    Ok(exit_code_for_status(status))
}

fn render_response(status: u16, status_text: &str, content_type: &str, body: &[u8]) -> String {
    if status < 400 {
        return compact_non_html(body, content_type);
    }

    let rendered_body = render_body(body, content_type, false);
    let status_label = if status_text.trim().is_empty() {
        status.to_string()
    } else {
        format!("{} {}", status, status_text.trim())
    };

    let header = format!(
        "[HTTP {}; {}; {} bytes]",
        status_label,
        type_hint(content_type),
        body.len()
    );

    if rendered_body.is_empty() {
        header
    } else {
        format!("{}\n{}", header, rendered_body)
    }
}

fn render_body(body: &[u8], content_type: &str, include_metadata: bool) -> String {
    let raw = match std::str::from_utf8(body) {
        Ok(raw) => Cow::Borrowed(raw),
        Err(_) if is_html_content_type(content_type) => String::from_utf8_lossy(body),
        Err(_) => return compact_non_utf8(body, content_type, include_metadata),
    };

    if is_html_content_type(content_type) || web_extract::is_html(&raw) {
        web_extract::extract_content(&raw)
    } else {
        compact_non_html_text(&raw, content_type, body.len(), include_metadata)
    }
}

fn compact_non_html(body: &[u8], content_type: &str) -> String {
    render_body(body, content_type, true)
}

fn compact_non_html_text(
    raw: &str,
    content_type: &str,
    byte_len: usize,
    include_metadata: bool,
) -> String {
    let trimmed = raw.trim();
    if trimmed.len() <= 500 {
        return trimmed.to_string();
    }

    let mut end = 500;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }

    if include_metadata {
        format!(
            "[{}; {} bytes]\n{}...",
            type_hint(content_type),
            byte_len,
            &trimmed[..end]
        )
    } else {
        format!("{}...", &trimmed[..end])
    }
}

fn compact_non_utf8(body: &[u8], content_type: &str, include_metadata: bool) -> String {
    if include_metadata {
        format!(
            "[{}; {} bytes; binary/non-UTF-8 body omitted]",
            type_hint(content_type),
            body.len()
        )
    } else {
        "[binary/non-UTF-8 body omitted]".to_string()
    }
}

fn is_html_content_type(content_type: &str) -> bool {
    content_type.to_ascii_lowercase().contains("html")
}

fn type_hint(content_type: &str) -> &str {
    if content_type.is_empty() {
        "unknown"
    } else {
        content_type
    }
}

fn exit_code_for_status(status: u16) -> i32 {
    if status >= 400 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_non_html_truncates_large_body() {
        let input = "x".repeat(700);
        let result = compact_non_html(input.as_bytes(), "text/plain");
        assert!(result.contains("text/plain"));
        assert!(result.contains("700 bytes"));
        assert!(result.len() < input.len());
    }

    #[test]
    fn compact_non_html_keeps_small_body() {
        let input = "hello";
        assert_eq!(compact_non_html(input.as_bytes(), "text/plain"), input);
    }

    #[test]
    fn compact_non_html_summarizes_non_utf8_body() {
        let result = compact_non_html(&[0, 159, 146, 150], "application/octet-stream");
        assert_eq!(
            result,
            "[application/octet-stream; 4 bytes; binary/non-UTF-8 body omitted]"
        );
    }

    #[test]
    fn render_response_includes_status_metadata_for_http_errors() {
        let result = render_response(404, "Not Found", "text/plain", b"missing");
        assert_eq!(result, "[HTTP 404 Not Found; text/plain; 7 bytes]\nmissing");
    }

    #[test]
    fn exit_code_for_status_preserves_http_error_failure_semantics() {
        assert_eq!(exit_code_for_status(200), 0);
        assert_eq!(exit_code_for_status(399), 0);
        assert_eq!(exit_code_for_status(400), 1);
        assert_eq!(exit_code_for_status(500), 1);
    }

    #[test]
    fn render_response_compacts_binary_http_error() {
        let result = render_response(
            500,
            "Internal Server Error",
            "application/octet-stream",
            &[0, 159, 146, 150],
        );
        assert_eq!(
            result,
            "[HTTP 500 Internal Server Error; application/octet-stream; 4 bytes]\n[binary/non-UTF-8 body omitted]"
        );
    }

    #[test]
    fn html_content_type_detection_is_case_insensitive() {
        assert!(is_html_content_type("Text/HTML; charset=utf-8"));
    }
}
