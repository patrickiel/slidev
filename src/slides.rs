//! A Slidev deck split into its slides, by the rules of `@slidev/parser`:
//! a line starting with `---` (outside a code block) starts a slide; when the
//! line after it isn't blank, the lines up to the next `---` are that slide's
//! frontmatter (the deck's headmatter, for the first), so `---`/yaml/`---`
//! is one break, not two. A slide may instead open with a ```` ```yaml ````
//! block. A slide with `src:` stands for the slides of another file.

use std::path::{Component, Path, PathBuf};

/// How deep `src:` imports are followed.
const MAX_DEPTH: usize = 3;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slide {
    /// The line (from 1) it starts on: its `---`, or 1 for the first.
    pub line: usize,
    /// Its frontmatter's top-level `key: value` lines, in order.
    pub frontmatter: Vec<(String, String)>,
    /// Its first heading, else its first line of text.
    pub heading: String,
    /// The comment that ends it: the presenter's notes.
    pub notes: String,
}

impl Slide {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.frontmatter
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// `title:`, else its heading.
    pub fn title(&self) -> &str {
        match self.get("title") {
            Some(title) if !title.is_empty() => title,
            _ if self.heading.is_empty() => "(untitled)",
            _ => &self.heading,
        }
    }

    pub fn layout(&self) -> &str {
        self.get("layout").unwrap_or_default()
    }

    /// `hide: true` or `disabled: true`: left out of the presentation.
    pub fn hidden(&self) -> bool {
        ["hide", "disabled"]
            .iter()
            .any(|key| self.get(key) == Some("true"))
    }

    fn src(&self) -> Option<&str> {
        self.get("src").filter(|src| !src.is_empty())
    }
}

/// The lines of one slide, before they are read.
struct Raw<'a> {
    start: usize,
    front: Option<Vec<&'a str>>,
    body: Vec<&'a str>,
}

impl Raw<'_> {
    fn at(start: usize) -> Self {
        Raw { start, front: None, body: Vec::new() }
    }
}

/// A code block's opening: its indent and backticks, which close it too.
fn fence(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start().len();
    let ticks = line[indent..].bytes().take_while(|b| *b == b'`').count();
    (ticks >= 3).then(|| &line[..indent + ticks])
}

/// The slides of a file's `text`.
pub fn parse(text: &str) -> Vec<Slide> {
    let lines: Vec<&str> = text.lines().collect();
    let mut raws = Vec::new();
    let mut cur = Raw::at(0);
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        if line.starts_with("---") {
            let done = std::mem::replace(&mut cur, Raw::at(i));
            raws.push(done);
            let next_blank = lines.get(i + 1).is_none_or(|l| l.trim().is_empty());
            if line.as_bytes().get(3) != Some(&b'-') && !next_blank {
                let end = (i + 1..lines.len())
                    .find(|&j| lines[j].trim_end() == "---")
                    .unwrap_or(lines.len());
                cur.front = Some(lines[i + 1..end].to_vec());
                i = end + 1;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(fence) = fence(lines[i])
            && let Some(end) = (i + 1..lines.len()).find(|&j| lines[j].starts_with(fence))
        {
            cur.body.extend(&lines[i..=end]);
            i = end + 1;
            continue;
        }
        cur.body.push(lines[i]);
        i += 1;
    }
    raws.push(cur);
    raws.into_iter()
        // A slide of no lines at all: before a `---` on the first line, after one on the last.
        .filter(|raw| raw.front.is_some() || !raw.body.is_empty())
        .map(read)
        .collect()
}

fn read(raw: Raw) -> Slide {
    let mut body = raw.body;
    let front = match raw.front {
        Some(front) => front,
        None => yaml_block(&mut body).unwrap_or_default(),
    };
    Slide {
        line: raw.start + 1,
        frontmatter: front.iter().filter_map(|line| key_value(line)).collect(),
        heading: heading(&body),
        notes: notes(&body),
    }
}

/// A ```` ```yaml ```` block that opens the slide: taken out of `body` as its frontmatter.
fn yaml_block<'a>(body: &mut Vec<&'a str>) -> Option<Vec<&'a str>> {
    let first = body.iter().position(|l| !l.trim().is_empty())?;
    let open = body[first].trim();
    if !(open.starts_with("```yaml") || open.starts_with("```yml")) {
        return None;
    }
    let end = (first + 1..body.len()).find(|&j| body[j].trim() == "```")?;
    let front = body[first + 1..end].to_vec();
    body.drain(first..=end);
    Some(front)
}

/// A top-level `key: value` line of YAML, its value unquoted.
fn key_value(line: &str) -> Option<(String, String)> {
    if line.starts_with(char::is_whitespace) || line.starts_with('#') || line.starts_with('-') {
        return None;
    }
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    let value = value.trim();
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|q| value.strip_prefix(*q).and_then(|v| v.strip_suffix(*q)))
        .unwrap_or(value);
    Some((key.to_string(), unquoted.to_string()))
}

/// The first heading outside code blocks, else the first line of text.
fn heading(body: &[&str]) -> String {
    let mut in_code: Option<&str> = None;
    let mut first_text = None;
    for line in body {
        if let Some(open) = in_code {
            if line.starts_with(open) {
                in_code = None;
            }
            continue;
        }
        if let Some(open) = fence(line) {
            in_code = Some(open);
            continue;
        }
        let text = line.trim();
        let hashes = text.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) && text[hashes..].starts_with(' ') {
            return plain(text[hashes..].trim().trim_end_matches('#').trim());
        }
        if first_text.is_none() && !text.is_empty() && !text.starts_with("<!--") {
            let text = plain(text);
            if !text.is_empty() {
                first_text = Some(text);
            }
        }
    }
    first_text.unwrap_or_default()
}

/// `text` without its HTML tags, at most 100 characters.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.trim();
    match out.char_indices().nth(100) {
        Some((ix, _)) => format!("{}…", &out[..ix]),
        None => out.to_string(),
    }
}

/// The comment the slide ends with.
fn notes(body: &[&str]) -> String {
    let text = body.join("\n");
    let text = text.trim_end();
    let Some(rest) = text.strip_suffix("-->") else {
        return String::new();
    };
    match rest.rfind("<!--") {
        Some(start) => rest[start + 4..].trim().to_string(),
        None => String::new(),
    }
}

/// A slide of the deck.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Its place in the deck, from 1, hidden slides counted.
    pub index: usize,
    /// Its number in the presentation (the page `/<no>`): hidden slides
    /// have none, and aren't counted.
    pub no: Option<usize>,
    /// The file it is in: the deck, or one it imports.
    pub file: PathBuf,
    pub slide: Slide,
}

/// A deck with what it imports.
#[derive(Debug, Default)]
pub struct Deck {
    pub entries: Vec<Entry>,
    /// The deck's file and the ones it imports.
    pub files: Vec<PathBuf>,
    /// Where each slide starts, by file and line, with its index: a `src:`
    /// slide's line stands for the first slide it imports.
    anchors: Vec<(PathBuf, usize, usize)>,
    /// The slides that show, so far.
    shown: usize,
}

impl Deck {
    /// The deck in file `entry`, its imports read from disk.
    pub fn load(entry: &Path) -> Deck {
        let mut deck = Deck::default();
        let entry = normalize(entry);
        if let Ok(text) = std::fs::read_to_string(&entry) {
            deck.collect(&entry, &entry, &text, 0, &[]);
        }
        deck
    }

    fn collect(&mut self, entry: &Path, file: &Path, text: &str, depth: usize, over: &[(String, String)]) {
        if !self.files.iter().any(|f| same_file(f, file)) {
            self.files.push(file.to_path_buf());
        }
        for (ix, mut slide) in parse(text).into_iter().enumerate() {
            if ix == 0 && !over.is_empty() {
                // The importing slide's frontmatter goes over the first imported one's.
                slide.frontmatter.extend(over.iter().cloned());
            }
            let imported = slide.src().map(|src| {
                let src = src.split('#').next().unwrap_or_default();
                let base = match src.strip_prefix('/') {
                    Some(_) => entry.parent(),
                    None => file.parent(),
                };
                normalize(&base.unwrap_or(Path::new("")).join(src.trim_start_matches('/')))
            });
            if let Some(path) = imported
                && depth < MAX_DEPTH
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                let first = self.entries.len() + 1;
                let over: Vec<_> = slide.frontmatter.iter().filter(|(k, _)| k != "src").cloned().collect();
                self.collect(entry, &path, &text, depth + 1, &over);
                if self.entries.len() >= first {
                    self.anchors.push((file.to_path_buf(), slide.line, first));
                }
                continue;
            }
            if let Some(src) = slide.src() {
                slide.heading = format!("{src} (not found)");
            }
            let index = self.entries.len() + 1;
            let no = (!slide.hidden()).then(|| {
                self.shown += 1;
                self.shown
            });
            self.anchors.push((file.to_path_buf(), slide.line, index));
            self.entries.push(Entry { index, no, file: file.to_path_buf(), slide });
        }
    }

    /// The index of the slide `line` (from 1) of `file` is in.
    pub fn slide_at(&self, file: &Path, line: usize) -> Option<usize> {
        self.anchors
            .iter()
            .filter(|(f, start, _)| *start <= line && same_file(f, file))
            .max_by_key(|(_, start, _)| *start)
            .or_else(|| self.anchors.iter().find(|(f, _, _)| same_file(f, file)))
            .map(|(_, _, no)| *no)
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index.checked_sub(1)?)
    }

    /// The page to show for slide `index`: its own, or for a hidden one the
    /// shown slide before it (else the first).
    pub fn page(&self, index: usize) -> usize {
        self.entries
            .iter()
            .take(index)
            .rev()
            .find_map(|e| e.no)
            .unwrap_or(1)
    }

    pub fn has_file(&self, file: &Path) -> bool {
        self.files.iter().any(|f| same_file(f, file))
    }
}

/// `path` without `.` and `..`, as far as they go.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// The same file, as den and the deck name it: separators and `.` aside,
/// and case too on Windows.
pub fn same_file(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| {
        let text = normalize(p).to_string_lossy().replace('\\', "/");
        if cfg!(windows) { text.to_lowercase() } else { text }
    };
    key(a) == key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(text: &str) -> Vec<(usize, String)> {
        parse(text).iter().map(|s| (s.line, s.title().to_string())).collect()
    }

    #[test]
    fn headmatter_and_separators() {
        let text = "---\ntheme: seriph\ntitle: My Talk\n---\n\n# Welcome\n\n---\n\n# Two\n\n---\n\n## Three ##\n";
        let slides = parse(text);
        assert_eq!(slides.len(), 3);
        assert_eq!(slides[0].get("theme"), Some("seriph"));
        // The deck's title is the first slide's too, as in Slidev.
        assert_eq!(titles(text), [(1, "My Talk".into()), (8, "Two".into()), (12, "Three".into())]);
    }

    #[test]
    fn frontmatter_after_a_separator_is_one_break() {
        let text = "# One\n\n---\nlayout: center\nclass: \"text-center\"\n---\n\n# Two\n---\n\n# Three";
        let slides = parse(text);
        assert_eq!(slides.len(), 3);
        assert_eq!(slides[1].layout(), "center");
        assert_eq!(slides[1].get("class"), Some("text-center"));
        assert_eq!(slides[1].line, 3);
        assert_eq!(slides[2].line, 9);
    }

    #[test]
    fn dashes_in_code_blocks_are_code() {
        let text = "# One\n\n```yaml\n---\nkey: value\n---\n```\n\n---\n\n# Two\n";
        assert_eq!(titles(text), [(1, "One".into()), (9, "Two".into())]);
        // An indented block closes at its own indent; a heading in it is no title.
        let text = "  ````md\n  # not this\n  ````\ntext line\n";
        assert_eq!(titles(text), [(1, "text line".into())]);
    }

    #[test]
    fn a_yaml_block_is_frontmatter() {
        let text = "# One\n\n---\n\n```yaml\nlayout: two-cols\n```\n\n# Two\n";
        let slides = parse(text);
        assert_eq!(slides[1].layout(), "two-cols");
        assert_eq!(slides[1].title(), "Two");
    }

    #[test]
    fn titles_fall_back_and_notes_end_a_slide() {
        let text = "<img src=\"a.png\">\nA <b>bold</b> line\n\n<!--\nSay hello.\n-->\n---\n\n<!-- only -->\n";
        let slides = parse(text);
        assert_eq!(slides[0].title(), "A bold line");
        assert_eq!(slides[0].notes, "Say hello.");
        assert_eq!(slides[1].title(), "(untitled)");
        assert_eq!(slides[1].notes, "only");
    }

    #[test]
    fn hidden_slides_and_no_empty_ends() {
        let text = "---\n---\n# A\n---\nhide: true\n---\n# B\n---\n";
        let slides = parse(text);
        assert_eq!(slides.len(), 2);
        assert!(!slides[0].hidden());
        assert!(slides[1].hidden());
    }

    #[test]
    fn imports_are_numbered_in_place() {
        let dir = std::env::temp_dir().join(format!("den-slidev-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("pages")).unwrap();
        std::fs::write(
            dir.join("slides.md"),
            "# One\n\n---\nsrc: ./pages/part.md\nlayout: cover\n---\n\n---\nhide: true\n---\n\n# Four\n\n---\nsrc: ./missing.md\n---\n",
        )
        .unwrap();
        std::fs::write(dir.join("pages/part.md"), "# Two\n\n---\n\n# Three\n").unwrap();
        let deck = Deck::load(&dir.join("slides.md"));
        let got: Vec<_> = deck.entries.iter().map(|e| (e.index, e.no, e.slide.title().to_string())).collect();
        assert_eq!(
            got,
            [
                (1, Some(1), "One".into()),
                (2, Some(2), "Two".into()),
                (3, Some(3), "Three".into()),
                (4, None, "Four".into()),
                (5, Some(4), "./missing.md (not found)".into())
            ]
        );
        assert_eq!(deck.entries[1].slide.layout(), "cover");
        // A hidden slide shows the page before it.
        assert_eq!((deck.page(4), deck.page(5)), (3, 4));
        assert!(deck.has_file(&dir.join("pages").join("..").join("pages/part.md")));
        // The deck's lines, and the imported file's.
        let deck_file = dir.join("slides.md");
        assert_eq!(deck.slide_at(&deck_file, 1), Some(1));
        assert_eq!(deck.slide_at(&deck_file, 5), Some(2));
        assert_eq!(deck.slide_at(&deck_file, 10), Some(4));
        assert_eq!(deck.slide_at(&dir.join("pages/part.md"), 4), Some(3));
        assert_eq!(deck.slide_at(&dir.join("other.md"), 1), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn paths_compare_as_files() {
        assert_eq!(normalize(Path::new("a/./b/../c")), PathBuf::from("a/c"));
        assert!(same_file(Path::new("x/./y.md"), Path::new("x/y.md")));
    }
}
