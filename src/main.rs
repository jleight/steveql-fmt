//! steveql-fmt: an opinionated SQL formatter.
//!
//! Parsing is delegated to sqlparser (PostgreSQL dialect); layout is our own.

mod format;
mod funcs;
mod lsp;
mod scan;

use std::io::{Read, Write};
use std::process::ExitCode;

use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};

use format::Comment;
use scan::{scan_not_equals, split_statements, swap_placeholders};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `steveql-fmt lsp` runs the language server over stdio.
    if args.first().map(String::as_str) == Some("lsp") {
        return lsp::run();
    }

    let mut opts = Opts::default();
    let mut files = Vec::new();
    for arg in &args {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            "-w" | "--write" => opts.write = true,
            "-c" | "--check" => opts.check = true,
            other if other.starts_with('-') && other != "-" => {
                eprintln!("steveql-fmt: unknown option '{other}'");
                return ExitCode::from(2);
            }
            other => files.push(other.to_string()),
        }
    }

    if opts.write && opts.check {
        eprintln!("steveql-fmt: --write and --check are mutually exclusive");
        return ExitCode::from(2);
    }

    if files.is_empty() {
        return run_stdin(&opts);
    }
    run_files(&files, &opts)
}

#[derive(Default)]
struct Opts {
    write: bool,
    check: bool,
}

fn run_stdin(opts: &Opts) -> ExitCode {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("steveql-fmt: failed to read stdin");
        return ExitCode::FAILURE;
    }
    let (formatted, warning) = format_sql(&input);
    if let Some(w) = &warning {
        eprintln!("steveql-fmt: <stdin>: {w}");
    }
    if opts.check {
        return if formatted == input {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    print!("{formatted}");
    ExitCode::SUCCESS
}

fn run_files(files: &[String], opts: &Opts) -> ExitCode {
    let mut had_error = false;
    let mut needs_format = false;

    for path in files {
        let original = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("steveql-fmt: {path}: {e}");
                had_error = true;
                continue;
            }
        };

        let (formatted, warning) = format_sql(&original);
        if let Some(w) = &warning {
            eprintln!("steveql-fmt: {path}: {w}");
        }

        if opts.check {
            if formatted != original {
                eprintln!("steveql-fmt: {path}: not formatted");
                needs_format = true;
            }
        } else if opts.write {
            if formatted != original {
                if let Err(e) = std::fs::write(path, &formatted) {
                    eprintln!("steveql-fmt: {path}: {e}");
                    had_error = true;
                }
            }
        } else {
            print!("{formatted}");
        }
    }

    if had_error {
        ExitCode::FAILURE
    } else if needs_format {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Format a SQL document. Returns the formatted text (with a single trailing
/// newline) and an optional warning. SQL we can't parse (e.g. SQLMesh `MODEL`
/// blocks) is passed through unchanged with a warning, so a file is never
/// destroyed.
pub(crate) fn format_sql(input: &str) -> (String, Option<String>) {
    // Split into statements first and format each independently, so a file that
    // mixes formattable SQL with something we can't parse (e.g. a SQLMesh
    // `MODEL (...)` block) still gets the parts we understand formatted, with
    // the rest passed through unchanged.
    let mut pieces = Vec::new();
    let mut errors = Vec::new();
    for (text, terminated) in split_statements(input) {
        if text.trim().is_empty() {
            continue;
        }
        let (mut piece, err) = format_statement(&text);
        if let Some(e) = err {
            errors.push(e);
        }
        if terminated {
            piece.push(';');
        }
        pieces.push(piece);
    }

    let mut body = pieces.join("\n\n");
    body.push('\n');

    let warning = (!errors.is_empty()).then(|| {
        format!(
            "left {} statement(s) unchanged: {}",
            errors.len(),
            errors.join("; ")
        )
    });
    (body, warning)
}

/// Format a single statement chunk. Returns the formatted text (no trailing
/// newline or semicolon) and, if it couldn't be parsed, the parse error (the
/// chunk is then passed through verbatim).
fn format_statement(chunk: &str) -> (String, Option<String>) {
    let trimmed = chunk.trim();
    let (prepared, placeholders) = swap_placeholders(trimmed);

    match Parser::parse_sql(&PostgreSqlDialect {}, &prepared) {
        Ok(statements) if !statements.is_empty() => {
            let fmt = format::Fmt::new(placeholders, collect_comments(trimmed));
            let body = statements
                .iter()
                .map(|s| fmt.statement(s))
                .collect::<Vec<_>>()
                .join("\n\n");
            // Restore not-equal spellings (and add 🤮 for `<>`) in source order.
            (
                format::voice_not_equal(&body, &scan_not_equals(trimmed)),
                None,
            )
        }
        // Empty parse (e.g. a comment-only chunk) — pass through, no error.
        Ok(_) => (trimmed.to_string(), None),
        Err(e) => (trimmed.to_string(), Some(e.to_string())),
    }
}

/// Collect comments from the source with their line numbers, flagging those
/// that trail code on the same line. sqlparser drops comments from its AST, so
/// the formatter re-attaches these by source position.
fn collect_comments(src: &str) -> Vec<Comment> {
    let tokens = match Tokenizer::new(&PostgreSqlDialect {}, src).tokenize_with_location() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut comments = Vec::new();
    let mut last_code_line: u64 = 0;
    for t in &tokens {
        match &t.token {
            Token::Whitespace(Whitespace::SingleLineComment { prefix, comment }) => {
                let text = format!("{}{}", prefix, comment.trim_end());
                comments.push(Comment {
                    line: t.span.start.line,
                    trailing: t.span.start.line == last_code_line,
                    text,
                });
            }
            Token::Whitespace(Whitespace::MultiLineComment(c)) => {
                comments.push(Comment {
                    line: t.span.start.line,
                    trailing: t.span.start.line == last_code_line,
                    text: format!("/*{c}*/"),
                });
            }
            Token::Whitespace(_) => {}
            _ => last_code_line = t.span.end.line,
        }
    }
    comments
}

fn print_help() {
    println!(
        "steveql-fmt — an opinionated SQL formatter\n\n\
         USAGE:\n    steveql-fmt [OPTIONS] [FILES]...\n    steveql-fmt lsp\n\n\
         With no FILES, reads SQL from stdin and writes the result to stdout.\n\
         The `lsp` subcommand runs a formatting language server over stdio.\n\n\
         OPTIONS:\n\
         \x20   -w, --write    Rewrite the given files in place\n\
         \x20   -c, --check    Exit non-zero if any file is not already formatted\n\
         \x20   -h, --help     Show this help"
    );
    let _ = std::io::stdout().flush();
}
