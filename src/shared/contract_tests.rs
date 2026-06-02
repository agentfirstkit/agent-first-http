#[test]
fn readme_and_reference_fetch_json_examples_are_flat() {
    for relative in ["README.md", "docs/reference.md"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let text = std::fs::read_to_string(&path).expect("read doc");
        let blocks = json_blocks(&text);
        let fetch = blocks
            .iter()
            .filter_map(|block| serde_json::from_str::<serde_json::Value>(block).ok())
            .find(|value| value.get("code").and_then(|v| v.as_str()) == Some("fetch"))
            .unwrap_or_else(|| panic!("no fetch JSON block in {}", path.display()));
        assert!(
            fetch.get("artifacts").is_none(),
            "fetch JSON example in {} must not nest *_file fields under artifacts",
            path.display()
        );
        for key in ["body_file", "rendered_html_file", "network_file"] {
            assert!(
                fetch.get(key).and_then(|v| v.as_str()).is_some(),
                "fetch JSON example in {} missing {key}",
                path.display()
            );
        }
    }
}

fn json_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_json = false;
    let mut current = String::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("```json") {
            in_json = true;
            current.clear();
            continue;
        }
        if in_json && line.trim_start().starts_with("```") {
            in_json = false;
            blocks.push(current.trim().to_string());
            current.clear();
            continue;
        }
        if in_json {
            current.push_str(line);
            current.push('\n');
        }
    }
    blocks
}
