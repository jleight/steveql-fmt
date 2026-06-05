//! A minimal, formatting-only Language Server over stdio.
//!
//! It advertises `documentFormattingProvider`, tracks open documents with full
//! text sync, and answers `textDocument/formatting` with a single whole-document
//! edit. On a parse error (or no change) it returns no edits, so the editor
//! leaves the buffer untouched.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use serde_json::{json, Value};

pub fn run() -> ExitCode {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    let mut docs: HashMap<String, String> = HashMap::new();
    let mut shutdown_requested = false;

    while let Some(msg) = read_message(&mut reader) {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();

        match method {
            "initialize" => {
                let result = json!({
                    "capabilities": {
                        "textDocumentSync": 1, // full document sync
                        "documentFormattingProvider": true
                    },
                    "serverInfo": {
                        "name": "steveql-fmt",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                });
                send_response(&mut writer, id, result);
            }
            "textDocument/didOpen" => {
                if let Some(td) = msg.pointer("/params/textDocument") {
                    if let (Some(uri), Some(text)) = (
                        td.get("uri").and_then(Value::as_str),
                        td.get("text").and_then(Value::as_str),
                    ) {
                        docs.insert(uri.to_string(), text.to_string());
                    }
                }
            }
            "textDocument/didChange" => {
                let uri = msg
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str);
                // Full sync: the last content change holds the whole document.
                let text = msg
                    .pointer("/params/contentChanges")
                    .and_then(Value::as_array)
                    .and_then(|a| a.last())
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str);
                if let (Some(uri), Some(text)) = (uri, text) {
                    docs.insert(uri.to_string(), text.to_string());
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = msg
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                {
                    docs.remove(uri);
                }
            }
            "textDocument/formatting" => {
                let edits = msg
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                    .and_then(|uri| docs.get(uri))
                    .and_then(|text| {
                        // Unparseable input passes through unchanged (no edit).
                        let (formatted, _warning) = crate::format_sql(text);
                        (&formatted != text).then(|| vec![full_edit(text, &formatted)])
                    })
                    .unwrap_or_default();
                send_response(&mut writer, id, Value::Array(edits));
            }
            "shutdown" => {
                shutdown_requested = true;
                send_response(&mut writer, id, Value::Null);
            }
            "exit" => {
                return if shutdown_requested {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }
            // Notifications we don't handle are ignored; unknown *requests*
            // (which carry an id) get a "method not found" error.
            _ => {
                if let Some(id) = id {
                    send_error(&mut writer, id, -32601, "method not found");
                }
            }
        }
    }

    ExitCode::SUCCESS
}

/// A whole-document replacement edit.
fn full_edit(old: &str, new: &str) -> Value {
    let (line, character) = end_position(old);
    json!({
        "range": {
            "start": { "line": 0, "character": 0 },
            "end": { "line": line, "character": character }
        },
        "newText": new
    })
}

/// The end position of `text` as an LSP position (line, UTF-16 column).
fn end_position(text: &str) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16();
        }
    }
    (line, col)
}

// ---------- JSON-RPC framing ----------

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let line = read_header_line(reader)?;
        if line.is_empty() {
            break; // blank line terminates the header block
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
        // Other headers (e.g. Content-Type) are ignored.
    }
    let len = content_length?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// Read one CRLF-terminated header line (without the terminator).
fn read_header_line(reader: &mut impl BufRead) -> Option<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                return if line.is_empty() {
                    None
                } else {
                    Some(strip_cr(line))
                }
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Some(strip_cr(line));
                }
                line.push(byte[0]);
            }
            Err(_) => return None,
        }
    }
}

fn strip_cr(mut line: Vec<u8>) -> String {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8_lossy(&line).into_owned()
}

fn send_response(writer: &mut impl Write, id: Option<Value>, result: Value) {
    write_message(
        writer,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    );
}

fn send_error(writer: &mut impl Write, id: Value, code: i64, message: &str) {
    write_message(
        writer,
        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    );
}

fn write_message(writer: &mut impl Write, msg: &Value) {
    let body = serde_json::to_string(msg).unwrap_or_default();
    let _ = write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = writer.flush();
}
