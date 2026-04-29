//! Extract readable text from HTML while dropping navigation and page chrome.

use scraper::{Html, Selector};

pub fn is_html(input: &str) -> bool {
    let trimmed = input.trim_start();
    trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<HTML")
}

pub fn extract_content(input: &str) -> String {
    if !is_html(input) {
        return input.to_string();
    }

    let document = Html::parse_document(input);
    let mut output = String::new();
    let main_selector = Selector::parse("main, article").expect("valid selector");

    if document.select(&main_selector).next().is_some() {
        for element in document.select(&main_selector) {
            extract_element_text(&element, &mut output);
        }
    } else if let Some(body) = document
        .select(&Selector::parse("body").expect("valid selector"))
        .next()
    {
        extract_element_text(&body, &mut output);
    } else {
        extract_element_text(&document.root_element(), &mut output);
    }

    clean_whitespace(&output)
}

fn clean_whitespace(output: &str) -> String {
    let mut result = Vec::new();
    let mut previous_blank = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !previous_blank {
                result.push(String::new());
            }
            previous_blank = true;
        } else {
            result.push(trimmed.to_string());
            previous_blank = false;
        }
    }

    result.join("\n").trim().to_string()
}

fn should_skip_tag(tag: &str) -> bool {
    matches!(
        tag,
        "nav" | "header" | "footer" | "aside" | "script" | "style" | "noscript"
    )
}

const NOISE_PATTERNS: &[&str] = &[
    "cookie",
    "consent",
    "banner",
    "newsletter",
    "subscribe",
    "signup",
    "social",
    "share",
    "follow",
    "ad-",
    "ad_",
    "ads",
    "advert",
    "advertisement",
    "sponsor",
];

fn has_noise_class_or_id(element: &scraper::ElementRef<'_>) -> bool {
    let class_attr = element.value().attr("class").unwrap_or("").to_lowercase();
    let id_attr = element.value().attr("id").unwrap_or("").to_lowercase();
    NOISE_PATTERNS
        .iter()
        .any(|pattern| class_attr.contains(pattern) || id_attr.contains(pattern))
}

fn extract_element_text(element: &scraper::ElementRef<'_>, output: &mut String) {
    for child in element.children() {
        match child.value() {
            scraper::node::Node::Text(text) => {
                let trimmed = text.text.trim();
                if !trimmed.is_empty() {
                    output.push_str(trimmed);
                    output.push('\n');
                }
            }
            scraper::node::Node::Element(el) => {
                let tag = el.name();
                if should_skip_tag(tag) {
                    continue;
                }

                if let Some(child_ref) = scraper::ElementRef::wrap(child) {
                    if has_noise_class_or_id(&child_ref) {
                        continue;
                    }

                    match tag {
                        "img" => {
                            if let Some(alt) =
                                el.attr("alt").map(str::trim).filter(|alt| !alt.is_empty())
                            {
                                output.push_str(&format!("[img: {}]\n", alt));
                            }
                        }
                        "pre" | "code" => {
                            let code = child_ref.text().collect::<Vec<_>>().join("");
                            let trimmed = code.trim();
                            if !trimmed.is_empty() {
                                output.push_str("```\n");
                                output.push_str(trimmed);
                                output.push_str("\n```\n");
                            }
                        }
                        "table" => extract_table(&child_ref, output),
                        _ => {
                            extract_element_text(&child_ref, output);
                            if is_block_tag(tag) {
                                output.push('\n');
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "div"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "ul"
            | "ol"
            | "section"
            | "blockquote"
            | "figure"
            | "figcaption"
            | "details"
            | "summary"
    )
}

fn extract_table(table: &scraper::ElementRef<'_>, output: &mut String) {
    let row_selector = Selector::parse("tr").expect("valid selector");
    let header_selector = Selector::parse("th").expect("valid selector");
    let cell_selector = Selector::parse("td").expect("valid selector");

    for row in table.select(&row_selector) {
        let cells: Vec<String> = row
            .select(&header_selector)
            .chain(row.select(&cell_selector))
            .map(|cell| cell.text().collect::<Vec<_>>().join("").trim().to_string())
            .filter(|cell| !cell.is_empty())
            .collect();

        if !cells.is_empty() {
            output.push_str(&cells.join(" | "));
            output.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_nav_footer_and_keeps_main() {
        let html = r#"<!DOCTYPE html><html><body><nav>Home</nav><main><h1>Main</h1><p>Article text.</p></main><footer>Copyright</footer></body></html>"#;
        let result = extract_content(html);
        assert!(result.contains("Main"));
        assert!(result.contains("Article text"));
        assert!(!result.contains("Home"));
        assert!(!result.contains("Copyright"));
    }

    #[test]
    fn preserves_code_tables_and_alt_text() {
        let html = r#"<!DOCTYPE html><html><body><main><pre><code>fn main() {}</code></pre><table><tr><th>Name</th><td>Alice</td></tr></table><img alt="diagram"></main></body></html>"#;
        let result = extract_content(html);
        assert!(result.contains("```"));
        assert!(result.contains("fn main"));
        assert!(result.contains("Name | Alice"));
        assert!(result.contains("[img: diagram]"));
    }

    #[test]
    fn non_html_passes_through() {
        let plain = "plain text";
        assert_eq!(extract_content(plain), plain);
    }
}
