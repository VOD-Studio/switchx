//! Best-effort syntax colors for editable text, without parsing or changing it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Plain,
    Key,
    String,
    Number,
    Literal,
    Comment,
    Section,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan<'a> {
    pub text: &'a str,
    pub prefix: &'a str,
    pub line_text: &'a str,
    pub line: i32,
    pub kind: TokenKind,
}

/// Colored fragments positioned over the original, unmodified editor text.
pub fn spans<'a>(source: &'a str, language: &str) -> Vec<HighlightSpan<'a>> {
    let mut output = Vec::new();
    let mut pos = 0;
    let mut line_start = 0;
    let mut line_end = source.find(['\r', '\n']).unwrap_or(source.len());
    let mut line = 0_i32;
    for (text, kind) in tokens(source, language) {
        let end = pos + text.len();
        let mut fragment_start = pos;
        while pos < end {
            if matches!(source.as_bytes()[pos], b'\r' | b'\n') {
                if kind != TokenKind::Plain && fragment_start < pos {
                    output.push(HighlightSpan {
                        text: &source[fragment_start..pos],
                        prefix: &source[line_start..fragment_start],
                        line_text: &source[line_start..line_end],
                        line,
                        kind,
                    });
                }
                if source.as_bytes()[pos] == b'\r'
                    || pos == 0
                    || source.as_bytes()[pos - 1] != b'\r'
                {
                    line = line.saturating_add(1);
                }
                pos += 1;
                line_start = pos;
                line_end = source[pos..]
                    .find(['\r', '\n'])
                    .map_or(source.len(), |end| pos + end);
                fragment_start = pos;
            } else {
                pos += 1;
            }
        }
        if kind != TokenKind::Plain && fragment_start < pos {
            output.push(HighlightSpan {
                text: &source[fragment_start..pos],
                prefix: &source[line_start..fragment_start],
                line_text: &source[line_start..line_end],
                line,
                kind,
            });
        }
    }
    output
}

pub fn tokens<'a>(source: &'a str, language: &str) -> Vec<(&'a str, TokenKind)> {
    if language.eq_ignore_ascii_case("json") {
        json_tokens(source)
    } else if language.eq_ignore_ascii_case("toml") {
        toml_tokens(source)
    } else if source.is_empty() {
        Vec::new()
    } else {
        vec![(source, TokenKind::Plain)]
    }
}

fn push<'a>(
    output: &mut Vec<(&'a str, TokenKind)>,
    source: &'a str,
    start: usize,
    end: usize,
    kind: TokenKind,
) {
    if let Some((previous, previous_kind)) = output.last_mut()
        && *previous_kind == kind
    {
        *previous = &source[start - previous.len()..end];
        return;
    }
    output.push((&source[start..end], kind));
}

fn json_tokens(source: &str) -> Vec<(&str, TokenKind)> {
    let bytes = source.as_bytes();
    let mut output = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let start = pos;
        let kind = if bytes[pos] == b'"' {
            pos = quoted_end(bytes, pos, false, false);
            let next = bytes[pos..]
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace());
            if next == Some(b':') {
                TokenKind::Key
            } else {
                TokenKind::String
            }
        } else if bytes[pos].is_ascii_whitespace() || b"{}[]:,".contains(&bytes[pos]) {
            pos += 1;
            TokenKind::Plain
        } else {
            while pos < bytes.len()
                && !bytes[pos].is_ascii_whitespace()
                && !b"{}[]:,\"".contains(&bytes[pos])
            {
                pos += 1;
            }
            let atom = &source[start..pos];
            if matches!(atom, "true" | "false" | "null") {
                TokenKind::Literal
            } else if atom
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_digit() || byte == b'-')
                && atom
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || b"-+.eE".contains(&byte))
            {
                TokenKind::Number
            } else {
                TokenKind::Plain
            }
        };
        push(&mut output, source, start, pos, kind);
    }
    output
}

fn toml_tokens(source: &str) -> Vec<(&str, TokenKind)> {
    let bytes = source.as_bytes();
    let mut output = Vec::new();
    let mut pos = 0;
    let mut statement_start = true;
    let mut expect_key = true;
    let mut containers = Vec::new();
    while pos < bytes.len() {
        let start = pos;
        let kind = if bytes[pos].is_ascii_whitespace() {
            if matches!(bytes[pos], b'\n' | b'\r') && containers.is_empty() {
                statement_start = true;
                expect_key = true;
            }
            pos += 1;
            TokenKind::Plain
        } else if bytes[pos] == b'#' {
            while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
                pos += 1;
            }
            TokenKind::Comment
        } else if statement_start && bytes[pos] == b'[' {
            pos += 1;
            while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r' | b'#') {
                if matches!(bytes[pos], b'"' | b'\'') {
                    pos = quoted_end(bytes, pos, false, true);
                } else if bytes[pos] == b']' {
                    pos += 1;
                    if bytes.get(pos) == Some(&b']') {
                        pos += 1;
                    }
                    break;
                } else {
                    pos += 1;
                }
            }
            statement_start = false;
            expect_key = false;
            TokenKind::Section
        } else if expect_key && let Some(end) = toml_key_end(bytes, pos) {
            pos = end;
            statement_start = false;
            expect_key = false;
            TokenKind::Key
        } else {
            statement_start = false;
            expect_key = false;
            match bytes[pos] {
                b'"' | b'\'' => {
                    pos = quoted_end(bytes, pos, true, true);
                    TokenKind::String
                }
                b'{' | b'[' => {
                    expect_key = bytes[pos] == b'{';
                    containers.push(bytes[pos]);
                    pos += 1;
                    TokenKind::Plain
                }
                b'}' | b']' => {
                    containers.pop();
                    pos += 1;
                    TokenKind::Plain
                }
                b',' => {
                    expect_key = containers.last() == Some(&b'{');
                    pos += 1;
                    TokenKind::Plain
                }
                b'=' | b'.' => {
                    pos += 1;
                    TokenKind::Plain
                }
                _ => {
                    while pos < bytes.len()
                        && !bytes[pos].is_ascii_whitespace()
                        && !b"{}[]=,#\"'".contains(&bytes[pos])
                    {
                        pos += 1;
                    }
                    let atom = &source[start..pos];
                    if matches!(atom, "true" | "false") {
                        TokenKind::Literal
                    } else if matches!(atom, "inf" | "+inf" | "-inf" | "nan" | "+nan" | "-nan")
                        || (atom.bytes().next().is_some_and(|byte| {
                            byte.is_ascii_digit() || matches!(byte, b'+' | b'-')
                        }) && atom.bytes().all(|byte| {
                            byte.is_ascii_digit() || b"_+-.:eEbBoOxXaAcCdDfFTtZz".contains(&byte)
                        }))
                    {
                        TokenKind::Number
                    } else {
                        TokenKind::Plain
                    }
                }
            }
        };
        push(&mut output, source, start, pos, kind);
    }
    output
}

fn quoted_end(bytes: &[u8], start: usize, allow_triple: bool, stop_at_newline: bool) -> usize {
    let quote = bytes[start];
    let triple = allow_triple && bytes.get(start..start + 3) == Some(&[quote; 3]);
    let mut pos = start + if triple { 3 } else { 1 };
    while pos < bytes.len() {
        if !triple && stop_at_newline && matches!(bytes[pos], b'\n' | b'\r') {
            break;
        }
        if bytes[pos] == b'\\' && quote == b'"' {
            pos += 1;
            if pos < bytes.len() {
                // The next delimiter is ASCII, so skipping its first byte is sufficient.
                pos += 1;
                while pos < bytes.len() && bytes[pos] & 0xc0 == 0x80 {
                    pos += 1;
                }
            }
        } else if bytes[pos] == quote {
            pos += 1;
            if !triple {
                break;
            }
            let run_start = pos - 1;
            while bytes.get(pos) == Some(&quote) {
                pos += 1;
            }
            if pos - run_start >= 3 {
                break;
            }
        } else {
            pos += 1;
        }
    }
    pos
}

fn toml_key_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut pos = start;
    loop {
        if matches!(bytes.get(pos), Some(b'"' | b'\'')) {
            let quote = bytes[pos];
            let end = quoted_end(bytes, pos, false, true);
            if end == pos + 1 || bytes.get(end - 1) != Some(&quote) {
                return None;
            }
            pos = end;
        } else {
            let component_start = pos;
            while bytes
                .get(pos)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                pos += 1;
            }
            if pos == component_start {
                return None;
            }
        }
        let key_end = pos;
        while matches!(bytes.get(pos), Some(b' ' | b'\t')) {
            pos += 1;
        }
        match bytes.get(pos) {
            Some(b'=') => return Some(key_end),
            Some(b'.') => {
                pos += 1;
                while matches!(bytes.get(pos), Some(b' ' | b'\t')) {
                    pos += 1;
                }
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<'a>(source: &'a str, language: &str) -> Vec<(&'a str, TokenKind)> {
        let result = tokens(source, language);
        assert_eq!(
            result.iter().map(|(text, _)| *text).collect::<String>(),
            source
        );
        assert!(result.iter().all(|(text, _)| !text.is_empty()));
        result
    }

    #[test]
    fn json_preserves_unicode_escapes_and_unfinished_values() {
        let result = check(
            r#"{"名字": "示例\"文字", "count": -12.5e+2, "enabled": true, "none": null, "open": "未完成\"#,
            "json",
        );
        assert!(result.contains(&(r#""名字""#, TokenKind::Key)));
        assert!(result.contains(&(r#""示例\"文字""#, TokenKind::String)));
        assert!(result.contains(&("-12.5e+2", TokenKind::Number)));
        assert!(result.contains(&("true", TokenKind::Literal)));
        assert_eq!(result.last(), Some(&(r#""未完成\"#, TokenKind::String)));
    }

    #[test]
    fn toml_distinguishes_tables_keys_arrays_and_comments() {
        let source = "[features.\"中文\"] # table\nvalues = [\"# value\", 1, true]\ninline = { name = '示例', nested.key = 1979-05-27T07:32:00Z }\n\"quoted.key\" . child = 0xFF\n";
        let result = check(source, "toml");
        assert!(result.contains(&("[features.\"中文\"]", TokenKind::Section)));
        assert!(result.contains(&("# table", TokenKind::Comment)));
        assert!(result.contains(&("\"# value\"", TokenKind::String)));
        assert!(result.contains(&("nested.key", TokenKind::Key)));
        assert!(result.contains(&("\"quoted.key\" . child", TokenKind::Key)));
        assert!(result.contains(&("1979-05-27T07:32:00Z", TokenKind::Number)));
        assert_eq!(
            result
                .iter()
                .filter(|(_, kind)| *kind == TokenKind::Section)
                .count(),
            1
        );
    }

    #[test]
    fn toml_multiline_strings_and_incomplete_input_remain_intact() {
        let source = "text = \"\"\"第一行\n# still a string\n\\\"内容\"\"\" # comment\nliteral = '''多行\n# literal'''\nunfinished = \"未完成\nnext = false\n";
        let result = check(source, "TOML");
        assert!(result.contains(&(
            "\"\"\"第一行\n# still a string\n\\\"内容\"\"\"",
            TokenKind::String
        )));
        assert!(result.contains(&("'''多行\n# literal'''", TokenKind::String)));
        assert!(result.contains(&("# comment", TokenKind::Comment)));
        assert!(result.contains(&("next", TokenKind::Key)));
        for source in ["", "中", "\"\\中", "[\"未完成", "key = { nested = [1,\n"] {
            check(source, "json");
            check(source, "toml");
        }
        let source = "\"名\" = \"\"\"第一行\r\n  第二行\n第三行\"\"\"\r\n  enabled = true\r\n";
        let positioned = spans(source, "toml");
        assert_eq!(positioned[1].line_text, "\"名\" = \"\"\"第一行");
        assert_eq!(positioned[2].line_text, "  第二行");
        assert_eq!(positioned[4].line_text, "  enabled = true");
        assert_eq!(
            positioned
                .iter()
                .map(|span| (span.text, span.prefix, span.line, span.kind))
                .collect::<Vec<_>>(),
            vec![
                ("\"名\"", "", 0, TokenKind::Key),
                ("\"\"\"第一行", "\"名\" = ", 0, TokenKind::String),
                ("  第二行", "", 1, TokenKind::String),
                ("第三行\"\"\"", "", 2, TokenKind::String),
                ("enabled", "  ", 3, TokenKind::Key),
                ("true", "  enabled = ", 3, TokenKind::Literal),
            ]
        );
    }
}
