//! End-to-end test of the `lsp` subcommand over stdio.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_steveql-fmt");

fn frame(v: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(v).unwrap();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Parse a stream of `Content-Length`-framed JSON messages.
fn parse_messages(mut bytes: &[u8]) -> Vec<Value> {
    let mut msgs = Vec::new();
    while let Some(pos) = find(bytes, b"\r\n\r\n") {
        let header = std::str::from_utf8(&bytes[..pos]).unwrap();
        let len: usize = header
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let start = pos + 4;
        let body = &bytes[start..start + len];
        msgs.push(serde_json::from_slice(body).unwrap());
        bytes = &bytes[start + len..];
    }
    msgs
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn lsp_formats_a_document() {
    let session = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
            "textDocument":{"uri":"file:///t.sql","languageId":"sql","version":1,
                "text":"select a,b from t where x<>1"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/formatting","params":{
            "textDocument":{"uri":"file:///t.sql"},
            "options":{"tabSize":4,"insertSpaces":true}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown"}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ];

    let mut input = Vec::new();
    for m in &session {
        input.extend_from_slice(&frame(m));
    }

    let mut child = Command::new(BIN)
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());

    let msgs = parse_messages(&out.stdout);

    // initialize advertises formatting support.
    let init = msgs
        .iter()
        .find(|m| m["id"] == json!(1))
        .expect("init reply");
    assert_eq!(
        init["result"]["capabilities"]["documentFormattingProvider"],
        json!(true)
    );

    // formatting returns a whole-document edit with the formatted text.
    let fmt = msgs
        .iter()
        .find(|m| m["id"] == json!(2))
        .expect("format reply");
    let edit = &fmt["result"][0];
    let new_text = edit["newText"].as_str().unwrap();
    assert!(
        new_text.contains("SELECT   a\n        ,b"),
        "got:\n{new_text}"
    );
    assert!(
        new_text.contains("WHERE   x <> 1  -- 🤮"),
        "got:\n{new_text}"
    );
    assert_eq!(edit["range"]["start"]["line"], json!(0));
}
