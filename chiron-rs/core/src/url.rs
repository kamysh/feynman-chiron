/// Rewrite BASE_URL's `options` query param to set `search_path` to SCHEMA
/// (plus ag_catalog, public), stripping any existing `options` param.
pub fn with_schema(base_url: &str, schema: &str) -> String {
    let base = if let Some(pos) = base_url.find('?') {
        &base_url[..pos]
    } else {
        base_url
    };
    let opts = urlencoding_simple(&format!("-c search_path={},ag_catalog,public", schema));
    format!("{}?options={}", base, opts)
}

/// Percent-encode just the characters that appear in a search_path options
/// string (space, =, ,) — not a general-purpose URL encoder.
pub fn urlencoding_simple(s: &str) -> String {
    s.chars().flat_map(|c| match c {
        ' ' => vec!['%', '2', '0'],
        '=' => vec!['%', '3', 'D'],
        ',' => vec!['%', '2', 'C'],
        c   => vec![c],
    }).collect()
}
