//! Small byte-search helpers used by inspectors.

pub(crate) fn count(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Case-insensitive search for an ASCII keyword at the start of any line.
pub(crate) fn has_line_starting_with(data: &[u8], keyword: &[u8]) -> bool {
    data.split(|b| *b == b'\n').any(|line| {
        let line = trim_start(line);
        line.len() >= keyword.len() && line[..keyword.len()].eq_ignore_ascii_case(keyword)
    })
}

pub(crate) fn trim_start(data: &[u8]) -> &[u8] {
    let start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(data.len());
    &data[start..]
}

/// Heuristic "this is a ZPL label" check, shared by inspectors of other languages to warn
/// about the most common mix-up.
pub(crate) fn looks_like_zpl(data: &[u8]) -> bool {
    contains(data, b"^XA") && contains(data, b"^XZ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(count(b"^XA^XZ^XA^XZ", b"^XA"), 2);
        assert_eq!(count(b"", b"^XA"), 0);
        assert!(has_line_starting_with(
            b"SIZE 4,3\r\n  print 1\r\n",
            b"PRINT"
        ));
        assert!(!has_line_starting_with(b"CLS\n", b"PRINT"));
    }
}
