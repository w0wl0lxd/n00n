use regex::Regex;

pub(super) const NO_MATCH: &str = "old_string not found in file";
pub(super) const MULTIPLE_MATCHES: &str =
    "old_string matches multiple locations; add surrounding context to make it unique";

const SINGLE_CANDIDATE_THRESHOLD: f64 = 0.0;
const MULTI_CANDIDATE_THRESHOLD: f64 = 0.3;
const CONTEXT_AWARE_LINE_MIN: usize = 3;
const CONTEXT_AWARE_MATCH_RATIO: f64 = 0.5;

type Replacer = fn(&str, &str) -> Vec<String>;

const REPLACERS: &[Replacer] = &[
    exact,
    line_trimmed,
    block_anchor,
    whitespace_normalized,
    indentation_flexible,
    escape_normalized,
    trimmed_boundary,
    context_aware,
];

pub(super) fn replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<String, String> {
    let mut any_found = false;

    for replacer in REPLACERS {
        for candidate in replacer(content, old_string) {
            let Some(first) = content.find(&candidate) else {
                continue;
            };
            any_found = true;

            if replace_all {
                return Ok(content.replace(&candidate, new_string));
            }

            if content[first + candidate.len()..].contains(&candidate) {
                continue;
            }

            let mut result = String::with_capacity(content.len() + new_string.len());
            result.push_str(&content[..first]);
            result.push_str(new_string);
            result.push_str(&content[first + candidate.len()..]);
            return Ok(result);
        }
    }

    if any_found {
        Err(MULTIPLE_MATCHES.into())
    } else {
        Err(NO_MATCH.into())
    }
}

fn exact(_content: &str, find: &str) -> Vec<String> {
    vec![find.to_string()]
}

fn line_trimmed(content: &str, find: &str) -> Vec<String> {
    let content_lines: Vec<&str> = content.split('\n').collect();
    let mut search_lines: Vec<&str> = find.split('\n').collect();
    if search_lines.last() == Some(&"") {
        search_lines.pop();
    }
    if search_lines.is_empty() || search_lines.len() > content_lines.len() {
        return vec![];
    }

    let mut results = Vec::new();
    for i in 0..=content_lines.len() - search_lines.len() {
        let all_match = search_lines
            .iter()
            .enumerate()
            .all(|(j, sl)| content_lines[i + j].trim() == sl.trim());
        if !all_match {
            continue;
        }

        results.push(content_lines[i..i + search_lines.len()].join("\n"));
    }
    results
}

fn indentation_flexible(content: &str, find: &str) -> Vec<String> {
    let find_lines: Vec<&str> = find.split('\n').collect();
    let content_lines: Vec<&str> = content.split('\n').collect();
    if find_lines.is_empty() || find_lines.len() > content_lines.len() {
        return vec![];
    }

    let normalized_find = strip_common_indent(&find_lines);
    let mut results = Vec::new();

    for i in 0..=content_lines.len() - find_lines.len() {
        let block = &content_lines[i..i + find_lines.len()];
        if strip_common_indent(block) == normalized_find {
            results.push(block.join("\n"));
        }
    }
    results
}

fn trimmed_boundary(content: &str, find: &str) -> Vec<String> {
    let trimmed = find.trim();
    if trimmed == find {
        return vec![];
    }

    let mut results = Vec::new();
    if content.contains(trimmed) {
        results.push(trimmed.to_string());
    }

    let find_lines: Vec<&str> = find.split('\n').collect();
    let content_lines: Vec<&str> = content.split('\n').collect();
    if find_lines.len() > 1 && find_lines.len() <= content_lines.len() {
        for i in 0..=content_lines.len() - find_lines.len() {
            let block = content_lines[i..i + find_lines.len()].join("\n");
            if block.trim() == trimmed {
                results.push(block);
            }
        }
    }
    results
}

fn block_anchor(content: &str, find: &str) -> Vec<String> {
    let content_lines: Vec<&str> = content.split('\n').collect();
    let mut search_lines: Vec<&str> = find.split('\n').collect();
    if search_lines.last() == Some(&"") {
        search_lines.pop();
    }
    if search_lines.len() < CONTEXT_AWARE_LINE_MIN {
        return vec![];
    }

    let first_trimmed = search_lines[0].trim();
    let last_trimmed = search_lines[search_lines.len() - 1].trim();

    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for (i, line) in content_lines.iter().enumerate() {
        if line.trim() != first_trimmed {
            continue;
        }
        if let Some(j) = content_lines[i + 2..]
            .iter()
            .position(|l| l.trim() == last_trimmed)
        {
            candidates.push((i, i + 2 + j));
        }
    }

    if candidates.is_empty() {
        return vec![];
    }

    let extract = |start: usize, end: usize| -> String { content_lines[start..=end].join("\n") };

    if candidates.len() == 1 {
        let (start, end) = candidates[0];
        let sim = middle_similarity(&content_lines[start..=end], &search_lines);
        return if sim >= SINGLE_CANDIDATE_THRESHOLD {
            vec![extract(start, end)]
        } else {
            vec![]
        };
    }

    let (best_start, best_end, best_sim) =
        candidates
            .iter()
            .fold((0, 0, -1.0_f64), |(bs, be, bsim), &(s, e)| {
                let sim = middle_similarity(&content_lines[s..=e], &search_lines);
                if sim > bsim {
                    (s, e, sim)
                } else {
                    (bs, be, bsim)
                }
            });

    if best_sim >= MULTI_CANDIDATE_THRESHOLD {
        vec![extract(best_start, best_end)]
    } else {
        vec![]
    }
}

fn whitespace_normalized(content: &str, find: &str) -> Vec<String> {
    let normalized_find = normalize_whitespace(find);
    let content_lines: Vec<&str> = content.split('\n').collect();
    let mut results = Vec::new();

    for line in &content_lines {
        if normalize_whitespace(line) == normalized_find {
            results.push(line.to_string());
            continue;
        }
        if let Some(matched) = substring_whitespace_match(line, &normalized_find) {
            results.push(matched);
        }
    }

    let find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.len() > 1 && find_lines.len() <= content_lines.len() {
        for i in 0..=content_lines.len() - find_lines.len() {
            let block = content_lines[i..i + find_lines.len()].join("\n");
            if normalize_whitespace(&block) == normalized_find {
                results.push(block);
            }
        }
    }

    results
}

fn escape_normalized(content: &str, find: &str) -> Vec<String> {
    let unescaped = unescape(find);
    if unescaped == find {
        return vec![];
    }

    let mut results = Vec::new();
    if content.contains(&unescaped) {
        results.push(unescaped.clone());
    }

    let content_lines: Vec<&str> = content.split('\n').collect();
    let find_lines: Vec<&str> = unescaped.split('\n').collect();
    if find_lines.len() > 1 && find_lines.len() <= content_lines.len() {
        for i in 0..=content_lines.len() - find_lines.len() {
            let block = content_lines[i..i + find_lines.len()].join("\n");
            if unescape(&block) == unescaped {
                results.push(block);
            }
        }
    }

    results
}

fn context_aware(content: &str, find: &str) -> Vec<String> {
    let content_lines: Vec<&str> = content.split('\n').collect();
    let mut find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.last() == Some(&"") {
        find_lines.pop();
    }
    if find_lines.len() < CONTEXT_AWARE_LINE_MIN {
        return vec![];
    }

    let first_trimmed = find_lines[0].trim();
    let last_trimmed = find_lines[find_lines.len() - 1].trim();
    let mut results = Vec::new();

    for (i, line) in content_lines.iter().enumerate() {
        if line.trim() != first_trimmed {
            continue;
        }
        let end = i + find_lines.len() - 1;
        if end >= content_lines.len() {
            continue;
        }
        if content_lines[end].trim() != last_trimmed {
            continue;
        }

        let block = &content_lines[i..=end];
        let (mut matching, mut total) = (0, 0);
        for k in 1..block.len() - 1 {
            let bl = block[k].trim();
            let fl = find_lines[k].trim();
            if !bl.is_empty() || !fl.is_empty() {
                total += 1;
                if bl == fl {
                    matching += 1;
                }
            }
        }

        if total == 0 || matching as f64 / total as f64 >= CONTEXT_AWARE_MATCH_RATIO {
            results.push(block.join("\n"));
        }
    }

    results
}

fn middle_similarity(block: &[&str], search: &[&str]) -> f64 {
    let block_mid = block.len().saturating_sub(2);
    let search_mid = search.len().saturating_sub(2);
    let lines_to_check = block_mid.min(search_mid);
    if lines_to_check == 0 {
        return 1.0;
    }

    let total: f64 = (1..=lines_to_check)
        .map(|j| {
            let a = block[j].trim();
            let b = search[j].trim();
            let max_len = a.len().max(b.len());
            if max_len == 0 {
                return 1.0;
            }
            1.0 - levenshtein(a, b) as f64 / max_len as f64
        })
        .sum();
    total / lines_to_check as f64
}

fn levenshtein(a: &str, b: &str) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0; b_chars.len() + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}

fn strip_common_indent(lines: &[&str]) -> String {
    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);

    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                *l
            } else {
                &l[min_indent..]
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws && !result.is_empty() {
                result.push(' ');
            }
            prev_ws = true;
        } else {
            prev_ws = false;
            result.push(ch);
        }
    }
    if result.ends_with(' ') {
        result.pop();
    }
    result
}

fn substring_whitespace_match(line: &str, normalized_find: &str) -> Option<String> {
    let normalized_line = normalize_whitespace(line);
    if !normalized_line.contains(normalized_find) || normalized_line == *normalized_find {
        return None;
    }

    let words: Vec<&str> = normalized_find.split(' ').collect();
    if words.is_empty() {
        return None;
    }

    let escaped: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
    let pattern = escaped.join(r"\s+");
    let re = Regex::new(&pattern).ok()?;
    re.find(line).map(|m| m.as_str().to_string())
}

fn unescape(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('\'') => result.push('\''),
                Some('"') => result.push('"'),
                Some('`') => result.push('`'),
                Some('\\') => result.push('\\'),
                Some('$') => result.push('$'),
                Some('\n') => result.push('\n'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    const R: &str = "REPLACED";

    #[test_case("fn foo() {}\nfn bar() {}", "fn foo() {}", R ; "exact")]
    #[test_case("fn foo() {}", "\nfn foo() {}\n", R ; "trimmed_boundary")]
    #[test_case("    fn f() {\n        bar();\n    }", "fn f() {\n    bar();\n}", R ; "different_indentation")]
    #[test_case("        fn f() {\n            bar();\n        }", "    fn f() {\n        bar();\n    }", R ; "shifted_block")]
    #[test_case("let   x  =   1;", "let x = 1;", R ; "whitespace_collapsed")]
    #[test_case("if\t(true)\t{ return; }", "if (true) { return; }", R ; "tabs_vs_spaces")]
    #[test_case("fn  foo()  {\n    bar();\n}", "fn foo() {\nbar();\n}", R ; "whitespace_multiline")]
    #[test_case("    let   x  =   compute(a,  b);", "compute(a, b)", R ; "whitespace_substring")]
    #[test_case("let s = \"hello\nworld\";", "let s = \"hello\\nworld\";", R ; "escaped_newline")]
    #[test_case("col1\tcol2\tcol3", "col1\\tcol2\\tcol3", R ; "escaped_tab")]
    #[test_case("fn test() {\n    let x = 1;\n    let y = 2;\n}", "fn test() {\n    let x = 99;\n    let y = 2;\n}", R ; "block_anchor_fuzzy_middle")]
    #[test_case("fn h() {\n    validate();\n    process();\n    save();\n    respond();\n}", "fn h() {\n    validate();\n    WRONG();\n    save();\n    respond();\n}", R ; "context_aware_partial_middle")]
    fn fuzzy_match_succeeds(content: &str, search: &str, replacement: &str) {
        assert!(
            replace(content, search, replacement, false)
                .unwrap()
                .contains(R)
        );
    }

    #[test_case("fn foo() {}", "MISSING", NO_MATCH ; "no_match")]
    #[test_case("let x = 1;\nlet x = 1;", "let x = 1;", MULTIPLE_MATCHES ; "ambiguous")]
    fn replace_rejects(content: &str, search: &str, expected_err: &str) {
        assert_eq!(
            replace(content, search, "x", false).unwrap_err(),
            expected_err
        );
    }

    #[test]
    fn replace_all() {
        let result = replace("let x = 1;\nlet x = 1;", "let x = 1;", "let x = 2;", true).unwrap();
        assert!(!result.contains("let x = 1;"));
    }

    #[test]
    fn block_anchor_picks_best_among_multiple() {
        let content = "fn a() {\n    unrelated();\n}\nfn a() {\n    target();\n}";
        let result = replace(content, "fn a() {\n    target();\n}", R, false).unwrap();
        assert!(result.contains(R));
        assert!(result.contains("unrelated()"));
    }

    #[test]
    fn leading_whitespace_disambiguates() {
        let result = replace("fn foo() {}\n  fn foo() {}", "  fn foo() {}", R, false).unwrap();
        assert!(result.starts_with("fn foo() {}"));
        assert!(result.ends_with(R));
    }

    #[test]
    fn context_aware_below_threshold_rejects() {
        let content = "fn f() {\n    a();\n    b();\n    c();\n    d();\n}";
        let search = "fn f() {\n    w();\n    x();\n    y();\n    z();\n}";
        assert!(context_aware(content, search).is_empty());
    }
}
