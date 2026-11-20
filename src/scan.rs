//! Lexical source scanning shared by the pre-parse passes.
//!
//! Several passes need to walk raw SQL while ignoring anything inside string
//! literals and comments — splitting on top-level `;`, recording not-equal
//! spellings, and swapping placeholders. They used to each carry their own
//! copy of the "skip strings and comments" loop, with subtly different escape
//! handling. [`Scanner`] is the single place that logic lives now: it yields
//! the source as a sequence of classified [`Chunk`]s, and each pass becomes a
//! small consumer that only inspects [`ChunkKind::Code`] regions.

/// The placeholder sentinel `swap_placeholders` substitutes for psycopg-style
/// `%s` / `%(name)s` and sqlc-style `@name` markers so sqlparser can parse the
/// statement.
pub const SENTINEL_PREFIX: &str = "$sqlfmtph";

/// What a [`Chunk`] of source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// Ordinary SQL — the only region the consumers inspect.
    Code,
    /// A single-quoted string literal, including its quotes (and any `''`
    /// escapes within).
    Str,
    /// A `--` line comment, up to but not including the newline.
    LineComment,
    /// A `/* ... */` block comment, including its delimiters.
    BlockComment,
}

/// A contiguous run of source of one [`ChunkKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub kind: ChunkKind,
    /// The slice of source this chunk covers.
    pub text: &'a str,
    /// Byte offset of `text` within the original source.
    pub start: usize,
}

/// Splits source into [`Chunk`]s. All delimiters (`'`, `--`, `/* */`) are
/// ASCII, so byte scanning never lands inside a multi-byte UTF-8 sequence and
/// every slice falls on a char boundary.
pub struct Scanner<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(src: &'a str) -> Self {
        Scanner { src, pos: 0 }
    }

    /// The kind of delimiter starting at byte `i`, if any.
    fn delim_at(&self, i: usize) -> Option<ChunkKind> {
        let b = self.src.as_bytes();
        match b[i] {
            b'\'' => Some(ChunkKind::Str),
            b'-' if b.get(i + 1) == Some(&b'-') => Some(ChunkKind::LineComment),
            b'/' if b.get(i + 1) == Some(&b'*') => Some(ChunkKind::BlockComment),
            _ => None,
        }
    }

    /// Advance past a string literal whose opening quote is at `self.pos`.
    /// A doubled quote (`''`) is an escape, not a terminator. An unterminated
    /// literal runs to end-of-source.
    fn consume_string(&mut self) {
        let b = self.src.as_bytes();
        self.pos += 1; // opening quote
        while self.pos < b.len() {
            if b[self.pos] == b'\'' {
                if b.get(self.pos + 1) == Some(&b'\'') {
                    self.pos += 2;
                    continue;
                }
                self.pos += 1; // closing quote
                return;
            }
            self.pos += 1;
        }
    }

    /// Advance to the newline ending a `--` comment (the newline is left for
    /// the next chunk).
    fn consume_line_comment(&mut self) {
        let b = self.src.as_bytes();
        while self.pos < b.len() && b[self.pos] != b'\n' {
            self.pos += 1;
        }
    }

    /// Advance past a `/* ... */` comment, including the closing `*/`. An
    /// unterminated comment runs to end-of-source.
    fn consume_block_comment(&mut self) {
        let b = self.src.as_bytes();
        self.pos += 2; // opening "/*"
        while self.pos < b.len() {
            if b[self.pos] == b'*' && b.get(self.pos + 1) == Some(&b'/') {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }

    /// Advance over ordinary SQL until the next string/comment delimiter or
    /// end-of-source.
    fn consume_code(&mut self) {
        while self.pos < self.src.len() && self.delim_at(self.pos).is_none() {
            self.pos += 1;
        }
    }
}

impl<'a> Iterator for Scanner<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Chunk<'a>> {
        if self.pos >= self.src.len() {
            return None;
        }
        let start = self.pos;
        let kind = match self.delim_at(start) {
            Some(ChunkKind::Str) => {
                self.consume_string();
                ChunkKind::Str
            }
            Some(ChunkKind::LineComment) => {
                self.consume_line_comment();
                ChunkKind::LineComment
            }
            Some(ChunkKind::BlockComment) => {
                self.consume_block_comment();
                ChunkKind::BlockComment
            }
            // `delim_at` only ever returns the three above; everything else
            // (including `None`) is code.
            _ => {
                self.consume_code();
                ChunkKind::Code
            }
        };
        Some(Chunk {
            kind,
            text: &self.src[start..self.pos],
            start,
        })
    }
}

/// Split SQL into statement chunks on top-level semicolons, returning each
/// chunk's text and whether it was terminated by a `;`. Semicolons inside
/// strings, comments, or brackets are ignored.
pub fn split_statements(src: &str) -> Vec<(String, bool)> {
    let mut chunks = Vec::new();
    let mut start = 0usize; // byte offset where the current statement begins
    let mut depth: i32 = 0;

    for chunk in Scanner::new(src) {
        if chunk.kind != ChunkKind::Code {
            continue;
        }
        let b = chunk.text.as_bytes();
        for (k, &byte) in b.iter().enumerate() {
            match byte {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b';' if depth <= 0 => {
                    let end = chunk.start + k;
                    chunks.push((src[start..end].to_string(), true));
                    start = end + 1;
                }
                _ => {}
            }
        }
    }

    if start < src.len() {
        let tail = &src[start..];
        if !tail.trim().is_empty() {
            chunks.push((tail.to_string(), false));
        }
    }
    chunks
}

/// Scan the source for not-equal operators (outside strings and comments) in
/// order, recording each spelling (true = `!=`, false = `<>`).
pub fn scan_not_equals(src: &str) -> Vec<bool> {
    let mut out = Vec::new();
    for chunk in Scanner::new(src) {
        if chunk.kind != ChunkKind::Code {
            continue;
        }
        let b = chunk.text.as_bytes();
        let mut k = 0;
        while k < b.len() {
            if b[k] == b'!' && b.get(k + 1) == Some(&b'=') {
                out.push(true);
                k += 2;
                continue;
            }
            if b[k] == b'<' && b.get(k + 1) == Some(&b'>') {
                out.push(false);
                k += 2;
                continue;
            }
            k += 1;
        }
    }
    out
}

/// Replace psycopg-style placeholders (`%s`, `%(name)s`) and sqlc/SQLMesh-style
/// ones (`@name`) with sqlparser-safe `$`-placeholders, returning the rewritten
/// SQL and the originals (indexed by sentinel number). String literals and
/// comments are left untouched.
pub fn swap_placeholders(src: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(src.len());
    let mut originals = Vec::new();

    for chunk in Scanner::new(src) {
        if chunk.kind != ChunkKind::Code {
            out.push_str(chunk.text);
            continue;
        }
        let b = chunk.text.as_bytes();
        let mut k = 0;
        let mut copied = 0; // bytes of this chunk already flushed to `out`
        while k < b.len() {
            // `%` followed by a letter or '('.
            let percent = b[k] == b'%'
                && matches!(b.get(k + 1), Some(&n) if n.is_ascii_alphabetic() || n == b'(');
            // `@name`, but not `@@name` or the `@` in `a@b`. Otherwise Postgres
            // reads it as the `@` (absolute value) operator, which sqlparser
            // prints with a space after it.
            let at = b[k] == b'@'
                && matches!(b.get(k + 1), Some(&n) if n.is_ascii_alphabetic() || n == b'_')
                && !(k > 0
                    && (b[k - 1].is_ascii_alphanumeric() || matches!(b[k - 1], b'_' | b'@')));
            if percent || at {
                out.push_str(&chunk.text[copied..k]);
                let pstart = k;
                k += 1;
                if b[k] == b'(' {
                    while k < b.len() && b[k] != b')' {
                        k += 1;
                    }
                    if k < b.len() {
                        k += 1; // consume ')'
                    }
                }
                while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                    k += 1;
                }
                originals.push(chunk.text[pstart..k].to_string());
                let idx = originals.len() - 1;
                out.push_str(&format!("{SENTINEL_PREFIX}{idx}"));
                copied = k;
                continue;
            }
            k += 1;
        }
        out.push_str(&chunk.text[copied..]);
    }

    (out, originals)
}

/// The comment that, directly above a statement, leaves it as written.
pub const IGNORE_DIRECTIVE: &str = "-- steveql-fmt: ignore";

/// Whether the comments leading a statement chunk, before any of its code,
/// include [`IGNORE_DIRECTIVE`].
pub fn has_ignore_directive(chunk: &str) -> bool {
    for c in Scanner::new(chunk) {
        match c.kind {
            ChunkKind::LineComment if c.text.trim_end() == IGNORE_DIRECTIVE => return true,
            ChunkKind::LineComment | ChunkKind::BlockComment => {}
            ChunkKind::Code if c.text.trim().is_empty() => {}
            _ => return false,
        }
    }
    false
}

/// Rewrite Postgres's `FOR NO KEY UPDATE` and `FOR KEY SHARE` locks, which
/// sqlparser can't parse, to `FOR UPDATE` and `FOR SHARE`. Returns the
/// rewritten SQL and, for every lock clause in order, whether it was one of
/// the `KEY` forms. String literals and comments are left untouched.
pub fn swap_lock_strengths(src: &str) -> (String, Vec<bool>) {
    let mut out = String::with_capacity(src.len());
    let mut keyed = Vec::new();

    for chunk in Scanner::new(src) {
        if chunk.kind != ChunkKind::Code {
            out.push_str(chunk.text);
            continue;
        }
        let text = chunk.text;
        let words = words(text);
        let mut copied = 0;
        for (i, &(start, end)) in words.iter().enumerate() {
            if !text[start..end].eq_ignore_ascii_case("FOR") {
                continue;
            }
            // The words after `FOR`, as long as only whitespace separates them.
            let mut prev = end;
            let next: Vec<(usize, usize, &str)> = words[i + 1..]
                .iter()
                .take(3)
                .take_while(|&&(s, e)| {
                    let gap = text[prev..s].trim().is_empty();
                    prev = e;
                    gap
                })
                .map(|&(s, e)| (s, e, &text[s..e]))
                .collect();
            let is = |j: usize, w: &str| next.get(j).is_some_and(|n| n.2.eq_ignore_ascii_case(w));
            if is(0, "NO") && is(1, "KEY") && is(2, "UPDATE") {
                out.push_str(&text[copied..next[0].0]);
                copied = next[2].0;
                keyed.push(true);
            } else if is(0, "KEY") && is(1, "SHARE") {
                out.push_str(&text[copied..next[0].0]);
                copied = next[1].0;
                keyed.push(true);
            } else if is(0, "UPDATE") || is(0, "SHARE") {
                keyed.push(false);
            }
        }
        out.push_str(&text[copied..]);
    }

    (out, keyed)
}

/// The byte ranges of the words (runs of ASCII letters, digits, `_` and `$`)
/// in `text`.
fn words(text: &str) -> Vec<(usize, usize)> {
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut k = 0;
    while k < b.len() {
        if is_word(b[k]) {
            let start = k;
            while k < b.len() && is_word(b[k]) {
                k += 1;
            }
            out.push((start, k));
        } else {
            k += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(ChunkKind, &str)> {
        Scanner::new(src).map(|c| (c.kind, c.text)).collect()
    }

    #[test]
    fn scanner_classifies_basic_regions() {
        use ChunkKind::*;
        assert_eq!(
            kinds("a 'b' -- c\nd /* e */ f"),
            vec![
                (Code, "a "),
                (Str, "'b'"),
                (Code, " "),
                (LineComment, "-- c"),
                (Code, "\nd "),
                (BlockComment, "/* e */"),
                (Code, " f"),
            ]
        );
    }

    #[test]
    fn scanner_handles_doubled_quote_escape() {
        use ChunkKind::*;
        // The `''` is an escape, so the literal does not end early.
        assert_eq!(kinds("'it''s' x"), vec![(Str, "'it''s'"), (Code, " x")]);
    }

    #[test]
    fn scanner_chunk_offsets_are_byte_accurate() {
        let src = "a 'b'";
        let chunks: Vec<_> = Scanner::new(src).collect();
        for c in &chunks {
            assert_eq!(&src[c.start..c.start + c.text.len()], c.text);
        }
    }

    #[test]
    fn scanner_survives_unterminated_string_and_comment() {
        use ChunkKind::*;
        assert_eq!(
            kinds("x 'unterminated"),
            vec![(Code, "x "), (Str, "'unterminated")]
        );
        assert_eq!(
            kinds("y /* open"),
            vec![(Code, "y "), (BlockComment, "/* open")]
        );
    }

    #[test]
    fn scanner_lone_dash_and_slash_are_code() {
        use ChunkKind::*;
        // A single `-` or `/` is arithmetic, not a comment.
        assert_eq!(kinds("a - b / c"), vec![(Code, "a - b / c")]);
    }

    #[test]
    fn scanner_preserves_multibyte_content() {
        use ChunkKind::*;
        // Non-ASCII inside a literal must not corrupt slicing.
        assert_eq!(kinds("'café' ✓"), vec![(Str, "'café'"), (Code, " ✓")]);
    }

    #[test]
    fn split_basic_statements() {
        assert_eq!(
            split_statements("select 1; select 2"),
            vec![
                ("select 1".to_string(), true),
                (" select 2".to_string(), false)
            ]
        );
    }

    #[test]
    fn split_trailing_semicolon_marks_terminated() {
        assert_eq!(
            split_statements("select 1;"),
            vec![("select 1".to_string(), true)]
        );
    }

    #[test]
    fn split_ignores_semicolons_in_strings_comments_and_brackets() {
        assert_eq!(
            split_statements("select ';'"),
            vec![("select ';'".to_string(), false)]
        );
        assert_eq!(
            split_statements("select 1 -- a; b\n"),
            vec![("select 1 -- a; b\n".to_string(), false)]
        );
        assert_eq!(
            split_statements("select arr[1;2]"),
            vec![("select arr[1;2]".to_string(), false)]
        );
        assert_eq!(
            split_statements("select (1;2)"),
            vec![("select (1;2)".to_string(), false)]
        );
    }

    #[test]
    fn split_blank_tail_is_dropped() {
        assert_eq!(
            split_statements("select 1;   \n"),
            vec![("select 1".to_string(), true)]
        );
    }

    #[test]
    fn not_equals_records_spellings_in_order() {
        assert_eq!(scan_not_equals("a != b and c <> d"), vec![true, false]);
    }

    #[test]
    fn not_equals_ignores_strings_and_comments() {
        assert_eq!(scan_not_equals("'a != b' -- c <> d\n"), Vec::<bool>::new());
        assert_eq!(scan_not_equals("x <> '!=' "), vec![false]);
    }

    #[test]
    fn placeholders_swap_positional_and_named() {
        let (out, originals) = swap_placeholders("where a = %s and b = %(name)s");
        assert_eq!(out, "where a = $sqlfmtph0 and b = $sqlfmtph1");
        assert_eq!(originals, vec!["%s".to_string(), "%(name)s".to_string()]);
    }

    #[test]
    fn placeholders_left_untouched_in_strings_and_comments() {
        let (out, originals) = swap_placeholders("a = '%s' -- %s\n");
        assert_eq!(out, "a = '%s' -- %s\n");
        assert!(originals.is_empty());
    }

    #[test]
    fn placeholders_preserve_surrounding_multibyte_code() {
        let (out, originals) = swap_placeholders("café = %s");
        assert_eq!(out, "café = $sqlfmtph0");
        assert_eq!(originals, vec!["%s".to_string()]);
    }

    #[test]
    fn placeholders_bare_percent_is_not_a_placeholder() {
        // `%` not followed by a letter or `(` (e.g. modulo with spaces) is left
        // alone.
        let (out, originals) = swap_placeholders("a % 2");
        assert_eq!(out, "a % 2");
        assert!(originals.is_empty());
    }

    #[test]
    fn ignore_directive_only_counts_before_code() {
        assert!(has_ignore_directive(
            "-- header\n-- steveql-fmt: ignore\nMODEL (x)"
        ));
        assert!(!has_ignore_directive("select 1 -- steveql-fmt: ignore\n"));
        assert!(!has_ignore_directive("-- steveql-fmt: ignored\nselect 1"));
    }

    #[test]
    fn lock_strengths_swap_key_forms() {
        let (out, keyed) = swap_lock_strengths(
            "for update; FOR NO KEY UPDATE OF t; for  key\nshare; for share; 'for key share'",
        );
        assert_eq!(
            out,
            "for update; FOR UPDATE OF t; for  share; for share; 'for key share'"
        );
        assert_eq!(keyed, vec![false, true, true, false]);
    }

    #[test]
    fn placeholders_swap_at_names() {
        let (out, originals) = swap_placeholders("a = @owner_id and b = @@x and c = d@e");
        assert_eq!(out, "a = $sqlfmtph0 and b = @@x and c = d@e");
        assert_eq!(originals, vec!["@owner_id".to_string()]);
    }
}
