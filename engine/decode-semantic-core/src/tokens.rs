//! BM25 tokenization (Semble `tokens.py`): ASCII identifiers, lowercased, with compound identifiers also split into
//! their camelCase / snake_case parts.

/// Splits one identifier into its lowercased form plus its parts when it has two or more: `HandlerStack` →
/// `handlerstack handler stack`, `my_func` → `my_func my func`, `simple` → `simple`.
pub(crate) fn split_identifier(token: &str) -> Vec<String> {
    let lower = token.to_ascii_lowercase();
    let parts: Vec<String> = if token.contains('_') {
        lower
            .split('_')
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        camel_parts(token)
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect()
    };
    if parts.len() >= 2 {
        let mut out = Vec::with_capacity(parts.len() + 1);
        out.push(lower);
        out.extend(parts);
        out
    } else {
        vec![lower]
    }
}

/// `re.findall(r"[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|[0-9]+", token)` on an ASCII identifier.
fn camel_parts(token: &str) -> Vec<&str> {
    let bytes = token.as_bytes();
    let at = |i: usize| bytes.get(i).copied().unwrap_or(0);
    let run = |mut i: usize, class: fn(&u8) -> bool| {
        while i < bytes.len() && class(&at(i)) {
            i += 1;
        }
        i
    };
    let mut parts = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = at(i);
        let end = if c.is_ascii_uppercase() {
            let upper_end = run(i, u8::is_ascii_uppercase);
            let upper_len = upper_end - i;
            if at(upper_end).is_ascii_lowercase() {
                if upper_len >= 2 {
                    // The acronym before a capitalised word: "XMLParser" → "XML".
                    upper_end - 1
                } else {
                    run(upper_end, u8::is_ascii_lowercase)
                }
            } else {
                upper_end
            }
        } else if c.is_ascii_lowercase() {
            run(i, u8::is_ascii_lowercase)
        } else if c.is_ascii_digit() {
            run(i, u8::is_ascii_digit)
        } else {
            i += 1;
            continue;
        };
        if let Some(part) = token.get(i..end) {
            parts.push(part);
        }
        i = end;
    }
    parts
}

/// Calls `each` with every `[a-zA-Z_][a-zA-Z0-9_]*` identifier in `text`.
pub(crate) fn for_each_identifier<'a>(text: &'a str, mut each: impl FnMut(&'a str)) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes.get(i).copied().unwrap_or(0);
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len()
                && bytes
                    .get(i)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                i += 1;
            }
            if let Some(identifier) = text.get(start..i) {
                each(identifier);
            }
        } else {
            i += 1;
        }
    }
}

/// Splits `text` into lowercase identifier tokens for BM25, compound identifiers followed by their parts.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for_each_identifier(text, |identifier| tokens.extend(split_identifier(identifier)));
    tokens
}

#[cfg(test)]
mod tests {
    use super::{split_identifier, tokenize};

    #[test]
    fn splits_like_semble() {
        assert_eq!(
            split_identifier("HandlerStack"),
            ["handlerstack", "handler", "stack"]
        );
        assert_eq!(
            split_identifier("getHTTPResponse"),
            ["gethttpresponse", "get", "http", "response"]
        );
        assert_eq!(split_identifier("XMLParser"), ["xmlparser", "xml", "parser"]);
        assert_eq!(split_identifier("my_func"), ["my_func", "my", "func"]);
        assert_eq!(split_identifier("__init__"), ["__init__"]);
        assert_eq!(split_identifier("simple"), ["simple"]);
        assert_eq!(split_identifier("ABC"), ["abc"]);
        assert_eq!(
            split_identifier("utf8Decode"),
            ["utf8decode", "utf", "8", "decode"]
        );
        assert_eq!(
            split_identifier("HTTPServer2X"),
            ["httpserver2x", "http", "server", "2", "x"]
        );
    }

    #[test]
    fn tokenizes_identifiers_only() {
        assert_eq!(
            tokenize("fn parseJSON(x: u8) -> é9a_b"),
            [
                "fn",
                "parsejson",
                "parse",
                "json",
                "x",
                "u8",
                "u",
                "8",
                "a_b",
                "a",
                "b"
            ]
        );
    }
}
