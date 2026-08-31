use std::fmt;

use ego_tree::NodeRef;
use scraper::{ElementRef, Html, Node, Selector};
use url::Url;

/// The deterministic extraction limits applied before parsing and after rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractLimits {
    /// Maximum number of UTF-8 bytes accepted from the already-decoded response body.
    pub max_body_bytes: usize,
    /// Maximum number of UTF-8 bytes in the returned Markdown.
    pub max_output_bytes: usize,
    /// Maximum number of distinct HTTP(S) links returned in document order.
    pub max_links: usize,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: 2 * 1024 * 1024,
            max_output_bytes: 2 * 1024 * 1024,
            max_links: 10_000,
        }
    }
}

/// A page's stable metadata, Markdown body, and discovered links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPage {
    pub title: String,
    pub markdown: String,
    pub links: Vec<String>,
}

/// Errors returned by the bounded HTML extraction pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractionError {
    InvalidLimit { name: &'static str },
    InvalidBaseUrl { url: String },
    InputTooLarge { limit: usize, actual: usize },
    OutputTooLarge { limit: usize, actual: usize },
    TooManyLinks { limit: usize },
    Conversion { message: String },
}

impl fmt::Display for ExtractionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit { name } => write!(formatter, "{name} must be greater than zero"),
            Self::InvalidBaseUrl { url } => write!(formatter, "invalid HTTP(S) base URL: {url}"),
            Self::InputTooLarge { limit, actual } => {
                write!(
                    formatter,
                    "HTML body is {actual} bytes, exceeding limit {limit}"
                )
            }
            Self::OutputTooLarge { limit, actual } => {
                write!(
                    formatter,
                    "Markdown output is {actual} bytes, exceeding limit {limit}"
                )
            }
            Self::TooManyLinks { limit } => {
                write!(
                    formatter,
                    "more than {limit} distinct links were discovered"
                )
            }
            Self::Conversion { message } => {
                write!(formatter, "HTML-to-Markdown conversion failed: {message}")
            }
        }
    }
}

impl std::error::Error for ExtractionError {}

/// Extract the selected content of an HTML document into deterministic Markdown.
///
/// The first `<main>` is selected, falling back to the first `<article>` and then
/// `<body>`. The parser recovers malformed HTML according to HTML5 rules.
pub fn extract_html(
    html: &str,
    base_url: &str,
    limits: ExtractLimits,
) -> Result<ExtractedPage, ExtractionError> {
    validate_limits(limits)?;
    if html.len() > limits.max_body_bytes {
        return Err(ExtractionError::InputTooLarge {
            limit: limits.max_body_bytes,
            actual: html.len(),
        });
    }

    let document_base =
        parse_http_url(base_url).ok_or_else(|| ExtractionError::InvalidBaseUrl {
            url: base_url.to_owned(),
        })?;
    let document = Html::parse_document(html);
    let effective_base = find_document_base(&document, &document_base);
    let title = find_title(&document);
    let selected = select_content(&document);

    let mut state = RenderState {
        base_url: &effective_base,
        links: Vec::new(),
        max_links: limits.max_links,
    };
    let mut sanitized_html = String::new();
    for child in selected.children() {
        serialize_node(child, &mut sanitized_html, &mut state)?;
    }

    let converter = htmd::HtmlToMarkdown::builder()
        .scripting_enabled(false)
        .build();
    let converted =
        converter
            .convert(&sanitized_html)
            .map_err(|error| ExtractionError::Conversion {
                message: error.to_string(),
            })?;
    let markdown = normalize_markdown(converted);
    if markdown.len() > limits.max_output_bytes {
        return Err(ExtractionError::OutputTooLarge {
            limit: limits.max_output_bytes,
            actual: markdown.len(),
        });
    }

    Ok(ExtractedPage {
        title,
        markdown,
        links: state.links,
    })
}

fn validate_limits(limits: ExtractLimits) -> Result<(), ExtractionError> {
    if limits.max_body_bytes == 0 {
        return Err(ExtractionError::InvalidLimit {
            name: "max_body_bytes",
        });
    }
    if limits.max_output_bytes == 0 {
        return Err(ExtractionError::InvalidLimit {
            name: "max_output_bytes",
        });
    }
    Ok(())
}

fn parse_http_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then_some(url)
}

fn resolve_http_url(raw: &str, base_url: &Url) -> Option<Url> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(absolute) = Url::parse(raw) {
        return (matches!(absolute.scheme(), "http" | "https") && absolute.host_str().is_some())
            .then_some(absolute);
    }
    let resolved = base_url.join(raw).ok()?;
    (matches!(resolved.scheme(), "http" | "https") && resolved.host_str().is_some())
        .then_some(resolved)
}

fn find_document_base(document: &Html, document_base: &Url) -> Url {
    let selector = Selector::parse("base[href]").expect("static base selector is valid");
    document
        .select(&selector)
        .filter_map(|element| element.attr("href"))
        .find_map(|href| resolve_http_url(href, document_base))
        .unwrap_or_else(|| document_base.clone())
}

fn find_title(document: &Html) -> String {
    let selector = Selector::parse("title").expect("static title selector is valid");
    document
        .select(&selector)
        .next()
        .map(|element| sanitize_metadata(&element.text().collect::<String>()))
        .unwrap_or_default()
}

fn select_content(document: &Html) -> ElementRef<'_> {
    // "main" wins outright.
    if let Some(element) = document
        .select(&Selector::parse("main").expect("valid selector"))
        .next()
    {
        return element;
    }
    // A lone <article> is the page; listing pages put one <article> per card,
    // so multiple articles mean the content root is <body> — selecting the
    // first card would silently drop every other item and its links.
    let article_selector = Selector::parse("article").expect("valid selector");
    let mut articles = document.select(&article_selector);
    if let (Some(first), None) = (articles.next(), articles.next()) {
        return first;
    }
    if let Some(element) = document
        .select(&Selector::parse("body").expect("valid selector"))
        .next()
    {
        return element;
    }
    document.root_element()
}

struct RenderState<'a> {
    base_url: &'a Url,
    links: Vec<String>,
    max_links: usize,
}

fn serialize_node(
    node: NodeRef<'_, Node>,
    output: &mut String,
    state: &mut RenderState<'_>,
) -> Result<(), ExtractionError> {
    match node.value() {
        Node::Text(text) => escape_text(output, text),
        Node::Element(element) => {
            let tag = element.name();
            if is_removed_tag(tag) {
                return Ok(());
            }

            let resolved_link = (tag == "a")
                .then(|| element.attr("href"))
                .flatten()
                .and_then(|href| resolve_http_url(href, state.base_url));
            if let Some(url) = resolved_link.as_ref() {
                let url = url.to_string();
                if !state.links.iter().any(|known| known == &url) {
                    if state.links.len() >= state.max_links {
                        return Err(ExtractionError::TooManyLinks {
                            limit: state.max_links,
                        });
                    }
                    state.links.push(url);
                }
            }

            output.push('<');
            output.push_str(tag);
            let mut attributes = Vec::new();
            for (name, value) in element.attrs() {
                if !is_preserved_attribute(name) || name == "href" || name == "src" {
                    continue;
                }
                attributes.push((name, value.to_owned()));
            }
            if tag == "a" {
                if let Some(url) = resolved_link {
                    attributes.push(("href", url.to_string()));
                }
            } else if tag == "img"
                && let Some(src) = element
                    .attr("src")
                    .and_then(|src| resolve_http_url(src, state.base_url))
            {
                attributes.push(("src", src.to_string()));
            }
            attributes.sort_unstable_by(|left, right| left.0.cmp(right.0));
            for (name, value) in attributes {
                output.push(' ');
                output.push_str(name);
                output.push_str("=\"");
                escape_attribute(output, &value);
                output.push('\"');
            }
            output.push('>');

            if !is_void_tag(tag) {
                for child in node.children() {
                    serialize_node(child, output, state)?;
                }
                output.push_str("</");
                output.push_str(tag);
                output.push('>');
            }
        }
        Node::Comment(_) | Node::Doctype(_) | Node::ProcessingInstruction(_) => {}
        Node::Document | Node::Fragment => {
            for child in node.children() {
                serialize_node(child, output, state)?;
            }
        }
    }
    Ok(())
}

fn is_removed_tag(tag: &str) -> bool {
    matches!(
        tag,
        "aside"
            | "base"
            | "dialog"
            | "footer"
            | "form"
            | "head"
            | "header"
            | "iframe"
            | "nav"
            | "noscript"
            | "script"
            | "style"
            | "template"
            | "title"
    )
}

fn is_void_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_preserved_attribute(name: &str) -> bool {
    matches!(
        name,
        "alt" | "class" | "colspan" | "height" | "rowspan" | "start" | "title" | "width"
    )
}

fn escape_text(output: &mut String, text: &str) {
    for character in sanitize_terminal(text).chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
}

fn escape_attribute(output: &mut String, value: &str) {
    for character in sanitize_terminal(value).chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
}

fn sanitize_terminal(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut characters = input.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            match characters.next() {
                Some('[') => {
                    for sequence_character in characters.by_ref() {
                        if ('@'..='~').contains(&sequence_character) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(sequence_character) = characters.next() {
                        if sequence_character == '\u{7}' {
                            break;
                        }
                        if sequence_character == '\u{1b}' && characters.next() == Some('\\') {
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            }
            continue;
        }
        if character == '\r' {
            output.push('\n');
        } else if character == '\n' || character == '\t' || !character.is_control() {
            output.push(character);
        }
    }
    output
}

fn sanitize_metadata(input: &str) -> String {
    sanitize_terminal(input)
        .chars()
        .filter(|character| *character != '\u{fffd}')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_markdown(markdown: String) -> String {
    let mut normalized = markdown.trim().to_owned();
    normalized.push('\n');
    normalized
}

#[cfg(test)]
mod tests {
    use super::{ExtractLimits, ExtractionError, extract_html};

    const BASE: &str = "https://example.com/root/page";

    fn limits() -> ExtractLimits {
        ExtractLimits {
            max_body_bytes: 100_000,
            max_output_bytes: 100_000,
            max_links: 100,
        }
    }

    #[test]
    fn multiple_articles_select_body_not_first_card() {
        // Real-world pattern: listing pages use one <article> per item card.
        // Selecting the first article would silently drop everything else.
        let html = r#"
            <html><head><title>List</title></head>
            <body>
              <h1>Product listing</h1>
              <article><h2><a href="/one">One</a></h2><p>first card</p></article>
              <article><h2><a href="/two">Two</a></h2><p>second card</p></article>
              <article><h2><a href="/three">Three</a></h2><p>third card</p></article>
              <nav>sidebar</nav>
            </body></html>
        "#;

        let page = extract_html(html, BASE, limits()).expect("HTML should extract");

        assert!(
            page.markdown.contains("Product listing"),
            "body heading kept"
        );
        for marker in ["first card", "second card", "third card"] {
            assert!(page.markdown.contains(marker), "missing {marker}");
        }
        for url in ["/one", "/two", "/three"] {
            assert!(
                page.links.iter().any(|l| l.ends_with(url)),
                "missing link {url}"
            );
        }
    }

    #[test]
    fn selects_main_and_removes_boilerplate() {
        let html = r#"
            <html><head><title>Example</title><style>.hidden { display:none }</style></head>
            <body>
              <nav>Navigation</nav><header>Header</header>
              <main><h1>Useful title</h1><p>Keep this paragraph.</p></main>
              <article><p>Do not select this article.</p></article>
              <aside>Related</aside><footer>Footer</footer><form>Login</form>
              <script>alert('no')</script><noscript>No script</noscript><template>Template</template>
            </body></html>
        "#;

        let page = extract_html(html, BASE, limits()).expect("HTML should extract");

        assert_eq!(page.title, "Example");
        assert!(page.markdown.contains("# Useful title"));
        assert!(page.markdown.contains("Keep this paragraph."));
        for removed in [
            "Navigation",
            "Header",
            "Do not select",
            "Related",
            "Footer",
            "Login",
            "alert",
            "No script",
            "Template",
            "hidden",
        ] {
            assert!(!page.markdown.contains(removed), "unexpected {removed:?}");
        }
    }

    #[test]
    fn falls_back_from_article_to_body() {
        let article = extract_html(
            "<body><div>Noise</div><article><h2>Article</h2><p>Chosen.</p></article></body>",
            BASE,
            limits(),
        )
        .expect("article should be selected");
        assert!(article.markdown.contains("Article"));
        assert!(article.markdown.contains("Chosen."));
        assert!(!article.markdown.contains("Noise"));

        let body = extract_html(
            "<body><div>Body content</div><footer>Footer</footer></body>",
            BASE,
            limits(),
        )
        .expect("body should be selected");
        assert!(body.markdown.contains("Body content"));
        assert!(!body.markdown.contains("Footer"));
    }

    #[test]
    fn malformed_html_and_unicode_are_decoded_deterministically() {
        let page = extract_html(
            "<main><h1>Привет &amp; café 🌍<h2>Next<p>Unicode <strong>текст</main>",
            BASE,
            limits(),
        )
        .expect("HTML5 parser should recover malformed markup");

        assert!(page.markdown.contains("Привет & café 🌍"));
        assert!(page.markdown.contains("Unicode **текст**"));
        assert_eq!(
            page.markdown.matches('\n').count(),
            page.markdown.lines().count()
        );
    }

    #[test]
    fn preserves_headings_paragraphs_emphasis_lists_tables_quotes_and_fenced_code() {
        let html = r#"
            <main>
              <h1>Heading</h1><p>A <em>small</em> and <strong>bold</strong> point.</p>
              <blockquote><p>Quoted line</p></blockquote>
              <ul><li>First</li><li>Second<ul><li>Nested</li></ul></li></ul>
              <ol><li>One</li><li>Two</li></ol>
              <table><thead><tr><th>Name</th><th>Value</th></tr></thead>
                <tbody><tr><td>A</td><td>1</td></tr><tr><td>B</td><td>2</td></tr></tbody>
              </table>
              <pre><code class="language-rust">fn main() {
    println!("hi");
}</code></pre>
            </main>
        "#;

        let page = extract_html(html, BASE, limits()).expect("rich HTML should extract");
        for expected in [
            "# Heading",
            "A *small* and **bold** point.",
            "> Quoted line",
            "*   First",
            "    *   Nested",
            "1.  One",
            "| Name | Value |",
            "| ---- | ----- |",
            "| A    | 1     |",
            "```rust",
            "println!(\"hi\");",
        ] {
            assert!(
                page.markdown.contains(expected),
                "missing {expected:?}, got:\n{}",
                page.markdown
            );
        }
    }

    #[test]
    fn resolves_links_using_base_and_keeps_first_duplicate_order() {
        let html = r##"
            <base href="/docs/">
            <main>
              <a href="guide">Guide</a>
              <a href="https://example.com/docs/guide">Duplicate</a>
              <a href="//cdn.example.net/asset">CDN</a>
              <a href="https://other.example/x#part">Absolute</a>
              <a href="#section">Fragment</a>
              <a href="javascript:alert(1)">Unsafe</a>
              <a href="mailto:user@example.com">Mail</a>
              <a href="data:text/plain,blocked">Data</a>
              <img src="/images/logo.png" alt="Logo">
            </main>
        "##;

        let page = extract_html(html, BASE, limits()).expect("links should resolve");

        assert_eq!(
            page.links,
            vec![
                "https://example.com/docs/guide",
                "https://cdn.example.net/asset",
                "https://other.example/x#part",
                "https://example.com/docs/#section",
            ]
        );
        assert!(
            page.markdown
                .contains("[Guide](https://example.com/docs/guide)")
        );
        assert!(
            page.markdown
                .contains("![Logo](https://example.com/images/logo.png)")
        );
        assert!(!page.markdown.contains("javascript:"));
        assert!(!page.markdown.contains("mailto:"));
        assert!(!page.markdown.contains("data:text"));
    }

    #[test]
    fn enforces_input_output_and_link_limits_with_typed_errors() {
        let mut small_input = limits();
        small_input.max_body_bytes = 3;
        assert!(matches!(
            extract_html("éé", BASE, small_input),
            Err(ExtractionError::InputTooLarge { .. })
        ));

        let mut small_output = limits();
        small_output.max_output_bytes = 5;
        assert!(matches!(
            extract_html("<main><p>long output</p></main>", BASE, small_output),
            Err(ExtractionError::OutputTooLarge { .. })
        ));

        let mut few_links = limits();
        few_links.max_links = 1;
        assert!(matches!(
            extract_html(
                "<main><a href='/a'>a</a><a href='/b'>b</a></main>",
                BASE,
                few_links
            ),
            Err(ExtractionError::TooManyLinks { .. })
        ));
    }

    #[test]
    fn emits_exactly_one_final_newline_and_sanitizes_title_metadata() {
        let page = extract_html(
            "<title>\u{1b}[31mAlert\u{1b}[0m\n\u{7} Title\u{0}</title><main><p>Text</p></main>",
            BASE,
            limits(),
        )
        .expect("metadata should be sanitized");

        assert_eq!(page.title, "Alert Title");
        assert!(!page.title.chars().any(char::is_control));
        assert!(!page.title.contains("[31m"));
        assert_eq!(
            page.markdown.trim_end_matches('\n').len() + 1,
            page.markdown.len()
        );
        assert!(page.markdown.ends_with('\n'));
    }
}
