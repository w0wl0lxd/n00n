pub(super) const NO_MATCH: &str = "old_string not found in file";
pub(super) const MULTIPLE_MATCHES: &str =
    "old_string matches multiple locations; add surrounding context to make it unique";

const MULTI_CANDIDATE_THRESHOLD: f64 = 0.3;
const CONTEXT_AWARE_LINE_MIN: usize = 3;

type Replacer = fn(&str, &str) -> Vec<String>;

const REPLACERS: &[Replacer] = &[
    exact,
    line_trimmed,
    indentation_flexible,
    trimmed_boundary,
    block_anchor,
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

        let matched: Vec<&str> = content_lines[i..i + search_lines.len()].to_vec();
        results.push(matched.join("\n"));
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
        return vec![extract(start, end)];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let content = "fn foo() {}\nfn bar() {}";
        let result = replace(content, "fn foo() {}", "fn baz() {}", false).unwrap();
        assert_eq!(result, "fn baz() {}\nfn bar() {}");
    }

    #[test]
    fn exact_match_replace_all() {
        let content = "let x = 1;\nlet x = 1;";
        let result = replace(content, "let x = 1;", "let x = 2;", true).unwrap();
        assert_eq!(result, "let x = 2;\nlet x = 2;");
    }

    #[test]
    fn no_match_returns_error() {
        let content = "fn foo() {}";
        assert_eq!(
            replace(content, "MISSING", "x", false).unwrap_err(),
            NO_MATCH
        );
    }

    #[test]
    fn ambiguous_exact_returns_error() {
        let content = "let x = 1;\nlet x = 1;";
        assert_eq!(
            replace(content, "let x = 1;", "let x = 2;", false).unwrap_err(),
            MULTIPLE_MATCHES
        );
    }

    #[test]
    fn line_trimmed_ignores_indentation() {
        let content = "    fn foo() {\n        bar();\n    }";
        let result = replace(content, "fn foo() {\n    bar();\n}", "REPLACED", false).unwrap();
        assert_eq!(result, "REPLACED");

        let content = "    if true {\n        x();\n    }";
        let result = replace(content, "if true {\n  x();\n}", "REPLACED", false).unwrap();
        assert_eq!(result, "REPLACED");
    }

    #[test]
    fn indentation_flexible_shifted_block() {
        let content = "        fn deep() {\n            body();\n        }";
        let search = "    fn deep() {\n        body();\n    }";
        let result = replace(content, search, "REPLACED", false).unwrap();
        assert_eq!(result, "REPLACED");
    }

    #[test]
    fn trimmed_boundary_extra_whitespace() {
        let content = "fn foo() {}";
        let search = "\nfn foo() {}\n";
        let result = replace(content, search, "fn bar() {}", false).unwrap();
        assert_eq!(result, "fn bar() {}");
    }

    #[test]
    fn block_anchor_fuzzy_middle() {
        let content = "fn test() {\n    let x = 1;\n    let y = 2;\n}";
        let search = "fn test() {\n    let x = 99;\n    let y = 2;\n}";
        let result = replace(content, search, "REPLACED", false).unwrap();
        assert_eq!(result, "REPLACED");
    }

    #[test]
    fn block_anchor_picks_best_among_multiple() {
        let content = "fn a() {\n    unrelated();\n}\nfn a() {\n    target();\n}";
        let search = "fn a() {\n    target();\n}";
        let result = replace(content, search, "REPLACED", false).unwrap();
        assert_eq!(result, "fn a() {\n    unrelated();\n}\nREPLACED");
    }

    #[test]
    fn exact_ambiguous_but_trimmed_unique_succeeds() {
        let content = "fn foo() {}\n  fn foo() {}";
        let result = replace(content, "  fn foo() {}", "REPLACED", false).unwrap();
        assert_eq!(result, "fn foo() {}\nREPLACED");
    }
}
