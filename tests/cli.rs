//! End-to-end tests driving the compiled binary.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_steveql-fmt");

/// Format `input` via stdin and return stdout (asserting a clean exit).
fn format(input: &str) -> String {
    let mut child = Command::new(BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "formatter failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Every file in `examples/` is already in canonical form, so formatting it is a
/// no-op. This both documents the style and guarantees the formatter is a
/// fixpoint (idempotent) over the whole showcase.
#[test]
fn examples_are_already_formatted() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("read examples/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no example files found");

    for path in paths {
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            format(&content),
            content,
            "{} is not in canonical form",
            path.file_name().unwrap().to_string_lossy()
        );
    }
}

#[test]
fn uppercases_keywords_and_standard_functions() {
    let out = format("select count(*), pg_table_size(oid) from t where x = true");
    assert!(out.contains("COUNT(*)"), "got:\n{out}");
    assert!(out.contains("pg_table_size(oid)"), "got:\n{out}");
    assert!(out.contains("= TRUE"), "got:\n{out}");
    assert!(out.contains("SELECT"), "got:\n{out}");
}

#[test]
fn group_by_ordinals_stay_inline_names_wrap() {
    let inline = format("select a, b from t group by 1, 2");
    assert!(inline.contains("GROUP BY 1, 2"), "got:\n{inline}");

    let wrapped = format("select a, b from t group by a, b");
    assert!(wrapped.contains("GROUP BY a\n"), "got:\n{wrapped}");
    assert!(wrapped.contains("        ,b"), "got:\n{wrapped}");
}

#[test]
fn not_equal_keeps_spelling_and_voices_displeasure() {
    let out = format("select a from t where x != 1 and y <> 2");
    assert!(out.contains("x != 1"), "got:\n{out}");
    assert!(out.contains("y <> 2  -- \u{1f92e}"), "got:\n{out}");
}

#[test]
fn preserves_comments() {
    let out = format("-- header\nselect a, -- trailing\n b from t\n");
    assert!(out.contains("-- header"), "got:\n{out}");
    assert!(out.contains("-- trailing"), "got:\n{out}");
}

#[test]
fn preserves_psycopg_placeholders() {
    let out = format("select a from t where x = any(%s) and y = %(name)s");
    assert!(out.contains("ANY(%s)"), "got:\n{out}");
    assert!(out.contains("%(name)s"), "got:\n{out}");
}

#[test]
fn unparseable_statement_passes_through_with_warning() {
    // A statement we can't parse (e.g. a SQLMesh MODEL block) is passed through
    // unchanged, with a warning on stderr and exit 0.
    let input = "MODEL (\n  name x.y\n)\n";
    let mut child = Command::new(BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), input);
    assert!(String::from_utf8(out.stderr).unwrap().contains("unchanged"));
}
