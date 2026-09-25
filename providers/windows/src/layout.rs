//! Device-independent text wrapping and pagination.
//!
//! The measuring function is injected so the algorithm is tested without GDI; on Windows
//! it is backed by `GetTextExtentPoint32W` on the printer DC.

/// Wraps one logical line into physical lines that satisfy `fits`. Breaks at spaces where
/// possible and inside words only when a single word is wider than the line.
pub fn wrap_line(line: &str, fits: &dyn Fn(&str) -> bool) -> Vec<String> {
    if line.is_empty() || fits(line) {
        return vec![line.to_owned()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for word in line.split_inclusive(' ') {
        let candidate = format!("{current}{word}");
        if fits(candidate.trim_end()) {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            out.push(current.trim_end().to_owned());
            current.clear();
        }
        if fits(word.trim_end()) {
            current.push_str(word);
        } else {
            let mut pieces = break_word(word, fits);
            current = pieces.pop().unwrap_or_default();
            out.extend(pieces);
        }
    }
    if !current.trim_end().is_empty() || out.is_empty() {
        out.push(current.trim_end().to_owned());
    }
    out
}

fn break_word(word: &str, fits: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    for c in word.chars() {
        current.push(c);
        // Always keep at least one character per line so we make progress.
        if !fits(current.trim_end()) && current.chars().count() > 1 {
            current.pop();
            pieces.push(std::mem::take(&mut current));
            current.push(c);
        }
    }
    pieces.push(current);
    pieces
}

/// Splits explicit pages into device pages holding at most `lines_per_page` lines each.
pub fn paginate(pages: Vec<Vec<String>>, lines_per_page: usize) -> Vec<Vec<String>> {
    let per_page = lines_per_page.max(1);
    let mut out = Vec::new();
    for page in pages {
        if page.is_empty() {
            out.push(Vec::new());
            continue;
        }
        out.extend(page.chunks(per_page).map(<[String]>::to_vec));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width(n: usize) -> impl Fn(&str) -> bool {
        move |s: &str| s.chars().count() <= n
    }

    #[test]
    fn short_lines_are_untouched() {
        assert_eq!(wrap_line("hello world", &width(20)), vec!["hello world"]);
        assert_eq!(wrap_line("", &width(5)), vec![""]);
    }

    #[test]
    fn wraps_at_word_boundaries() {
        assert_eq!(
            wrap_line("the quick brown fox", &width(10)),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn breaks_long_words() {
        assert_eq!(
            wrap_line("abcdefghij xy", &width(4)),
            vec!["abcd", "efgh", "ij", "xy"]
        );
    }

    #[test]
    fn narrow_width_still_makes_progress() {
        assert_eq!(wrap_line("abc", &width(0)), vec!["a", "b", "c"]);
    }

    #[test]
    fn paginates_and_keeps_blank_pages() {
        let lines = |n: usize| (0..n).map(|i| i.to_string()).collect::<Vec<_>>();
        let pages = paginate(vec![lines(5), vec![], lines(2)], 2);
        assert_eq!(pages.len(), 5);
        assert_eq!(pages[2], vec!["4"]);
        assert!(pages[3].is_empty());
    }
}
