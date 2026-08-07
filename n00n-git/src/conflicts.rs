use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gix::bstr::BString;
use gix::dir as gix_dir;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::error::GitError;

const DEFAULT_MAX_HUNK_LINES: usize = 8;
const DEFAULT_MAX_FILE_BYTES: usize = 1_048_576;
const CONFLICT_MARKER_MIN_LEN: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Conflict,
    Todo,
    Fixme,
    Hack,
    Placeholder,
}

impl std::str::FromStr for FindingKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "conflict" => Ok(Self::Conflict),
            "todo" => Ok(Self::Todo),
            "fixme" => Ok(Self::Fixme),
            "hack" => Ok(Self::Hack),
            "placeholder" => Ok(Self::Placeholder),
            _ => Err(format!("unknown finding kind: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    Compact,
    Full,
    #[default]
    Both,
}

impl std::str::FromStr for OutputMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "compact" => Ok(Self::Compact),
            "full" => Ok(Self::Full),
            "both" => Ok(Self::Both),
            _ => Err(format!("unknown output mode: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictsOptions {
    pub kinds: Vec<FindingKind>,
    pub output: OutputMode,
    pub max_hunk_lines: usize,
    pub max_file_bytes: usize,
    pub include_untracked: bool,
    pub include_ignored: bool,
}

impl Default for ConflictsOptions {
    fn default() -> Self {
        Self {
            kinds: vec![
                FindingKind::Conflict,
                FindingKind::Todo,
                FindingKind::Fixme,
                FindingKind::Hack,
                FindingKind::Placeholder,
            ],
            output: OutputMode::Both,
            max_hunk_lines: DEFAULT_MAX_HUNK_LINES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            include_untracked: true,
            include_ignored: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConflicts {
    pub files: Vec<ConflictFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictFile {
    pub path: String,
    pub findings: Vec<Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub kind: String,
    pub line: u32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hunk: Option<ConflictHunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictHunk {
    pub start_line: u32,
    pub end_line: u32,
    pub ours_label: Option<String>,
    pub base_label: Option<String>,
    pub theirs_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ours: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theirs: Option<Vec<String>>,
}

/// Find conflict markers, TODO/FIXME/HACK comments, and placeholder comments in a worktree.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened, is bare, or the directory walk fails.
#[instrument(skip(path, options))]
pub fn find(path: &Path, options: &ConflictsOptions) -> Result<GitConflicts, GitError> {
    let repo = gix::open(path)
        .map_err(|e| GitError::GitOperation(format!("failed to open repository: {e}")))?;
    let worktree = repo.worktree().ok_or(GitError::BareRepo)?;
    let workdir = worktree
        .base()
        .canonicalize()
        .map_err(|e| GitError::GitOperation(format!("failed to canonicalize workdir: {e}")))?;

    let index = repo
        .index()
        .map_err(|e| GitError::GitOperation(format!("failed to load index: {e}")))?;

    let mut dirwalk_options = repo
        .dirwalk_options()
        .map_err(|e| GitError::GitOperation(format!("failed to create dirwalk options: {e}")))?
        .emit_tracked(true)
        .emit_untracked(gix_dir::walk::EmissionMode::Matching);

    if options.include_ignored {
        dirwalk_options = dirwalk_options.emit_ignored(Some(gix_dir::walk::EmissionMode::Matching));
    }

    let mut collect = Collect {
        entries: Vec::new(),
    };
    let should_interrupt = AtomicBool::new(false);
    repo.dirwalk(
        &index,
        Vec::<BString>::new(),
        &should_interrupt,
        dirwalk_options,
        &mut collect,
    )
    .map_err(|e| GitError::GitOperation(format!("dirwalk failed: {e}")))?;

    let mut files = Vec::new();

    for entry in collect.entries {
        if !entry_is_file(&entry) {
            continue;
        }

        let rela_path = &entry.rela_path;
        if !status_is_included(entry.status, options) {
            continue;
        }

        let rel_bstr: &gix::bstr::BStr = rela_path.as_ref();
        let is_unmerged = is_unmerged(&index, rel_bstr);

        let rel = gix::path::try_from_bstr(rela_path)
            .map_err(|e| GitError::GitOperation(format!("invalid relative path: {e}")))?;
        let file_path = workdir.join(rel.as_ref());
        let canonical_file = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.clone());
        if !canonical_file.starts_with(&workdir) {
            tracing::warn!(
                path = %file_path.display(),
                "skipping file outside worktree"
            );
            continue;
        }

        match scan_file(&file_path, options, is_unmerged) {
            Ok(mut conflict_file) => {
                if !conflict_file.findings.is_empty() || conflict_file.truncated.is_some() {
                    conflict_file.path = String::from_utf8_lossy(rela_path.as_slice()).into_owned();
                    files.push(conflict_file);
                }
            }
            Err(e) => {
                tracing::warn!(path = %file_path.display(), error = %e, "skipping file");
            }
        }
    }

    Ok(GitConflicts { files })
}

fn entry_is_file(entry: &gix_dir::Entry) -> bool {
    matches!(
        entry.disk_kind.or(entry.index_kind),
        Some(gix_dir::entry::Kind::File)
    )
}

fn status_is_included(status: gix_dir::entry::Status, options: &ConflictsOptions) -> bool {
    match status {
        gix_dir::entry::Status::Tracked => true,
        gix_dir::entry::Status::Untracked => options.include_untracked,
        gix_dir::entry::Status::Ignored(_) => options.include_ignored,
        gix_dir::entry::Status::Pruned => false,
    }
}

struct Collect {
    entries: Vec<gix_dir::Entry>,
}

impl gix_dir::walk::Delegate for Collect {
    fn emit(
        &mut self,
        entry: gix_dir::EntryRef<'_>,
        _collapsed_directory_status: Option<gix_dir::entry::Status>,
    ) -> gix_dir::walk::Action {
        self.entries.push(entry.to_owned());
        std::ops::ControlFlow::Continue(())
    }
}

fn is_unmerged(index: &gix::worktree::Index, rela_path: &gix::bstr::BStr) -> bool {
    index
        .entry_by_path_and_stage(rela_path, gix::index::entry::Stage::Ours)
        .is_some()
        || index
            .entry_by_path_and_stage(rela_path, gix::index::entry::Stage::Theirs)
            .is_some()
}

#[instrument(skip(file_path, options), fields(path = %file_path.display()))]
fn scan_file(
    file_path: &Path,
    options: &ConflictsOptions,
    is_unmerged: bool,
) -> Result<ConflictFile, GitError> {
    let file = File::open(file_path)
        .map_err(|e| GitError::GitOperation(format!("failed to open file: {e}")))?;
    let mut reader = BufReader::new(file);

    let mut findings = Vec::new();
    let mut parser = MarkerParser::new(options);
    let mut line_no = 0u32;
    let mut bytes_read = 0usize;
    let mut truncated = false;

    if is_unmerged && options.kinds.contains(&FindingKind::Conflict) {
        // `line: 0` is a sentinel for a file-level unmerged index entry, since there
        // is no concrete content/line number to report.
        findings.push(Finding {
            kind: "conflict".to_string(),
            line: 0,
            message: "unmerged index entry".to_string(),
            content: None,
            hunk: None,
        });
    }

    loop {
        let mut buf = Vec::new();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| GitError::GitOperation(format!("failed to read file: {e}")))?;
        if n == 0 {
            break;
        }

        bytes_read = bytes_read.saturating_add(n);
        line_no = line_no.saturating_add(1);

        if bytes_read > options.max_file_bytes {
            truncated = true;
            break;
        }

        let line = trim_newline(&buf);

        if is_binary(line) {
            break;
        }

        if let Some(finding) = parser.process_line(line_no, line) {
            findings.push(finding);
        }

        if let Some(smell) = detect_smell(line_no, line, options) {
            findings.push(smell);
        }
    }

    if let Some(finding) = parser.finish() {
        findings.push(finding);
    }

    Ok(ConflictFile {
        path: String::new(),
        findings,
        truncated: truncated.then_some(true),
    })
}

fn trim_newline(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    if end > 0 && buf[end - 1] == b'\n' {
        end -= 1;
        if end > 0 && buf[end - 1] == b'\r' {
            end -= 1;
        }
    }
    &buf[..end]
}

fn is_binary(line: &[u8]) -> bool {
    line.contains(&0)
}

struct MarkerParser<'a> {
    options: &'a ConflictsOptions,
    state: ParseState,
    start_line: u32,
    ours_label: Option<String>,
    base_label: Option<String>,
    theirs_label: Option<String>,
    ours: Vec<String>,
    base: Vec<String>,
    theirs: Vec<String>,
}

#[derive(Clone, Copy)]
enum ParseState {
    Neutral,
    Ours,
    Base,
    Theirs,
}

impl<'a> MarkerParser<'a> {
    fn new(options: &'a ConflictsOptions) -> Self {
        Self {
            options,
            state: ParseState::Neutral,
            start_line: 0,
            ours_label: None,
            base_label: None,
            theirs_label: None,
            ours: Vec::new(),
            base: Vec::new(),
            theirs: Vec::new(),
        }
    }

    fn process_line(&mut self, line_no: u32, line: &[u8]) -> Option<Finding> {
        if let Some((marker, label)) = parse_marker(line) {
            match (marker, self.state) {
                (MarkerKind::Start, ParseState::Neutral) => {
                    self.state = ParseState::Ours;
                    self.start_line = line_no;
                    self.ours_label = label.map(String::from_utf8_lossy).map(Into::into);
                    None
                }
                (MarkerKind::Start, _) => {
                    let finding = self.build_finding(line_no, None);
                    self.state = ParseState::Ours;
                    self.start_line = line_no;
                    self.ours_label = label.map(String::from_utf8_lossy).map(Into::into);
                    self.base_label = None;
                    self.theirs_label = None;
                    self.ours.clear();
                    self.base.clear();
                    self.theirs.clear();
                    finding
                }
                (MarkerKind::Base, ParseState::Ours) => {
                    self.state = ParseState::Base;
                    self.base_label = label.map(String::from_utf8_lossy).map(Into::into);
                    None
                }
                (MarkerKind::Separator, ParseState::Ours | ParseState::Base) => {
                    self.state = ParseState::Theirs;
                    None
                }
                (MarkerKind::End, ParseState::Theirs) => {
                    let finding = self.build_finding(line_no, label);
                    self.reset();
                    finding
                }
                _ => {
                    self.reset();
                    None
                }
            }
        } else {
            match self.state {
                ParseState::Ours => self.ours.push(line_to_string(line)),
                ParseState::Base => self.base.push(line_to_string(line)),
                ParseState::Theirs => self.theirs.push(line_to_string(line)),
                ParseState::Neutral => {}
            }
            None
        }
    }

    fn finish(&mut self) -> Option<Finding> {
        if !matches!(self.state, ParseState::Neutral) {
            let line = self.start_line.saturating_add(self.length_estimate());
            return self.build_finding(line, None);
        }
        None
    }

    fn length_estimate(&self) -> u32 {
        let total = self
            .ours
            .len()
            .saturating_add(self.base.len())
            .saturating_add(self.theirs.len());
        u32::try_from(total).unwrap_or_else(|_| u32::MAX)
    }

    fn build_finding(&self, end_line: u32, label: Option<&[u8]>) -> Option<Finding> {
        if !self.options.kinds.contains(&FindingKind::Conflict) {
            return None;
        }

        let theirs_label = label.map(String::from_utf8_lossy).map(Into::into);
        let hunk_len = self.ours.len() + self.base.len() + self.theirs.len();

        let (ours, base, theirs) = match self.options.output {
            OutputMode::Full => (
                Some(self.ours.clone()),
                Some(self.base.clone()),
                Some(self.theirs.clone()),
            ),
            OutputMode::Compact => (None, None, None),
            OutputMode::Both => {
                if hunk_len <= self.options.max_hunk_lines {
                    (
                        Some(self.ours.clone()),
                        Some(self.base.clone()),
                        Some(self.theirs.clone()),
                    )
                } else {
                    (None, None, None)
                }
            }
        };

        Some(Finding {
            kind: "conflict".to_string(),
            line: self.start_line,
            message: format!("conflict marker at line {}", self.start_line),
            content: None,
            hunk: Some(ConflictHunk {
                start_line: self.start_line,
                end_line,
                ours_label: self.ours_label.clone(),
                base_label: self.base_label.clone(),
                theirs_label: theirs_label.or_else(|| self.theirs_label.clone()),
                ours,
                base,
                theirs,
            }),
        })
    }

    fn reset(&mut self) {
        self.state = ParseState::Neutral;
        self.start_line = 0;
        self.ours_label = None;
        self.base_label = None;
        self.theirs_label = None;
        self.ours.clear();
        self.base.clear();
        self.theirs.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    Start,
    Base,
    Separator,
    End,
}

fn parse_marker(line: &[u8]) -> Option<(MarkerKind, Option<&[u8]>)> {
    if line.is_empty() {
        return None;
    }

    let first = line[0];
    let marker = match first {
        b'<' => MarkerKind::Start,
        b'|' => MarkerKind::Base,
        b'=' => MarkerKind::Separator,
        b'>' => MarkerKind::End,
        _ => return None,
    };

    let mut count = 0usize;
    for &b in line {
        if b == first {
            count += 1;
        } else {
            break;
        }
    }

    if count < CONFLICT_MARKER_MIN_LEN {
        return None;
    }

    let rest = &line[count..];
    let label = if rest.is_empty() {
        None
    } else if rest[0] == b' ' {
        Some(&rest[1..])
    } else if rest.starts_with(b": ") {
        Some(&rest[2..])
    } else {
        None
    };

    Some((marker, label))
}

fn line_to_string(line: &[u8]) -> String {
    String::from_utf8_lossy(line).into_owned()
}

fn detect_smell(line_no: u32, line: &[u8], options: &ConflictsOptions) -> Option<Finding> {
    if !is_comment_line(line) {
        return None;
    }

    let line_str = String::from_utf8_lossy(line);
    let lower = line_str.to_lowercase();

    if let Some(kind) = detect_smell_kind(&lower, options) {
        let message = match kind {
            FindingKind::Todo => "TODO comment",
            FindingKind::Fixme => "FIXME comment",
            FindingKind::Hack => "HACK comment",
            FindingKind::Placeholder => "placeholder comment",
            FindingKind::Conflict => return None,
        };

        let content = match options.output {
            OutputMode::Compact => None,
            OutputMode::Full | OutputMode::Both => Some(line_str.into_owned()),
        };

        return Some(Finding {
            kind: format!("{kind:?}").to_lowercase(),
            line: line_no,
            message: message.to_string(),
            content,
            hunk: None,
        });
    }

    None
}

fn detect_smell_kind(lower: &str, options: &ConflictsOptions) -> Option<FindingKind> {
    let has_word = |pat: &str| lower.contains(pat);

    if options.kinds.contains(&FindingKind::Todo) && has_word("todo") {
        return Some(FindingKind::Todo);
    }
    if options.kinds.contains(&FindingKind::Fixme) && has_word("fixme") {
        return Some(FindingKind::Fixme);
    }
    if options.kinds.contains(&FindingKind::Hack) && has_word("hack") {
        return Some(FindingKind::Hack);
    }
    if options.kinds.contains(&FindingKind::Placeholder) {
        for pat in PLACEHOLDER_PATTERNS {
            if lower.contains(pat) {
                return Some(FindingKind::Placeholder);
            }
        }
    }
    None
}

const PLACEHOLDER_PATTERNS: &[&str] = &[
    "for now",
    "in a real",
    "an actual",
    "in production",
    "we should",
    "not implemented",
    "placeholder",
    "stub",
    "temporary",
    "workaround",
    "fix this later",
    "remove this",
    "incomplete",
    "to be implemented",
    "tbd",
    "only for",
    "not final",
];

fn is_comment_line(line: &[u8]) -> bool {
    let trimmed = trim_leading_whitespace(line);
    if trimmed.starts_with(b"//") {
        return true;
    }
    if trimmed.starts_with(b"#") && !trimmed.starts_with(b"#{") && !trimmed.starts_with(b"#[") {
        return true;
    }
    if trimmed.starts_with(b"-- ") || trimmed.starts_with(b"--[") {
        return true;
    }
    if trimmed.starts_with(b"/*") || trimmed.starts_with(b"*/") {
        return true;
    }
    if trimmed.starts_with(b"* ") {
        return true;
    }
    if trimmed.starts_with(b"<!--") {
        return true;
    }
    if trimmed.starts_with(b"REM ") || trimmed.starts_with(b"rem ") {
        return true;
    }
    false
}

fn trim_leading_whitespace(line: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t') {
        i += 1;
    }
    &line[i..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_conflict_text() -> Vec<u8> {
        let mut text = Vec::new();
        text.extend_from_slice(b"line one\n");
        text.extend_from_slice(&[b'<'; CONFLICT_MARKER_MIN_LEN]);
        text.extend_from_slice(b" HEAD\n");
        text.extend_from_slice(b"ours\n");
        text.extend_from_slice(&[b'='; CONFLICT_MARKER_MIN_LEN]);
        text.push(b'\n');
        text.extend_from_slice(b"theirs\n");
        text.extend_from_slice(&[b'>'; CONFLICT_MARKER_MIN_LEN]);
        text.extend_from_slice(b" branch\n");
        text.extend_from_slice(b"line after");
        text
    }

    #[test]
    fn parse_simple_conflict() {
        let text = build_conflict_text();

        let options = ConflictsOptions::default();
        let mut parser = MarkerParser::new(&options);
        let mut findings = Vec::new();
        for (i, line) in text.split(|&b| b == b'\n').enumerate() {
            if let Some(f) = parser.process_line(u32::try_from(i + 1).unwrap(), line) {
                findings.push(f);
            }
        }
        if let Some(f) = parser.finish() {
            findings.push(f);
        }

        assert_eq!(findings.len(), 1);
        let hunk = findings[0].hunk.as_ref().unwrap();
        assert_eq!(hunk.start_line, 2);
        assert_eq!(hunk.ours, Some(vec!["ours".to_string()]));
        assert_eq!(hunk.theirs, Some(vec!["theirs".to_string()]));
        assert_eq!(hunk.ours_label, Some("HEAD".to_string()));
        assert_eq!(hunk.theirs_label, Some("branch".to_string()));
    }

    #[test]
    fn detect_todo_in_comment() {
        let options = ConflictsOptions::default();
        let line = b"// TODO: fix this";
        let finding = detect_smell(1, line, &options).unwrap();
        assert_eq!(finding.kind, "todo");
        assert_eq!(finding.line, 1);
    }

    #[test]
    fn detect_placeholder() {
        let options = ConflictsOptions::default();
        let line = b"# use a real database in production";
        let finding = detect_smell(1, line, &options).unwrap();
        assert_eq!(finding.kind, "placeholder");
    }

    #[test]
    fn find_conflicts_in_repo() {
        use std::process::Command;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        std::fs::write(
            root.join(".gitconfig"),
            "[user]\nname = Test\nemail = test@example.com\n",
        )
        .unwrap();

        let run = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(root)
                .env("HOME", root)
                .env("XDG_CONFIG_HOME", root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };

        run(&["init"]);
        std::fs::write(root.join("file.txt"), "line one\nline two\n").unwrap();
        run(&["add", "file.txt"]);
        run(&["commit", "-m", "base"]);

        let default_branch =
            String::from_utf8(run(&["rev-parse", "--abbrev-ref", "HEAD"]).stdout).unwrap();

        run(&["checkout", "-b", "theirs"]);
        std::fs::write(root.join("file.txt"), "line one\nline two the theirs\n").unwrap();
        run(&["commit", "-am", "theirs"]);

        run(&["checkout", default_branch.trim()]);
        std::fs::write(root.join("file.txt"), "line one\nline two the ours\n").unwrap();
        run(&["commit", "-am", "ours"]);

        // Merge may fail with conflicts; that is the desired state.
        let _ = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["merge", "theirs"])
            .output();

        let options = ConflictsOptions::default();
        let result = find(root, &options).unwrap();

        assert!(!result.files.is_empty());
        let file = result
            .files
            .iter()
            .find(|f| f.path == "file.txt")
            .expect("file.txt in conflicts");
        assert!(file.findings.iter().any(|f| f.kind == "conflict"));
    }
}
