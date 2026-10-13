//! `phase lsp` end to end: a client session over real stdin/stdout pipes.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

/// Read one framed message from the server.
fn read_message(r: &mut impl BufRead) -> String {
    let mut len = 0;
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse().unwrap();
        }
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[test]
fn a_session_publishes_diagnostics_lists_symbols_and_shuts_down() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_phase"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |body: &str| stdin.write_all(frame(body).as_bytes()).unwrap();

    send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#);
    let init = read_message(&mut stdout);
    assert!(init.contains(r#""id":1"#), "{init}");
    assert!(init.contains(r#""documentSymbolProvider":true"#), "{init}");
    assert!(init.contains(r#""change":1"#), "{init}");
    send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);

    // A program reading a buffer the DMA engine still owns.
    let bad = "extern fn look(x: read buffer<u8, 4> @RAM);\nfn main() {\n    buffer<u8, 4> b @RAM;\n    move b -> @DMA;\n    look(b);\n    sync(b);\n}\n";
    send(&format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///t.phase","languageId":"phase","version":1,"text":"{}"}}}}}}"#,
        escape(bad)
    ));
    let diags = read_message(&mut stdout);
    assert!(diags.contains("textDocument/publishDiagnostics"), "{diags}");
    assert!(
        diags.contains("entity 'b' is in domain @DMA but 'look' expects @RAM"),
        "{diags}"
    );
    // On line 5 (0-based 4), underlining `b` in `look(b)`.
    assert!(
        diags.contains(r#""start":{"character":9,"line":4}"#),
        "{diags}"
    );
    assert!(diags.contains("relatedInformation"), "{diags}");

    // Fixing it clears the diagnostics.
    let good = bad.replace(
        "    look(b);\n    sync(b);\n",
        "    sync(b);\n    look(b);\n",
    );
    send(&format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"file:///t.phase","version":2}},"contentChanges":[{{"text":"{}"}}]}}}}"#,
        escape(&good)
    ));
    let cleared = read_message(&mut stdout);
    assert!(cleared.contains(r#""diagnostics":[]"#), "{cleared}");

    send(
        r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":"file:///t.phase"}}}"#,
    );
    let symbols = read_message(&mut stdout);
    assert!(symbols.contains(r#""name":"look""#), "{symbols}");
    assert!(symbols.contains(r#""name":"main""#), "{symbols}");

    send(r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{}}"#);
    let unsupported = read_message(&mut stdout);
    assert!(unsupported.contains("-32601"), "{unsupported}");

    send(r#"{"jsonrpc":"2.0","id":4,"method":"shutdown"}"#);
    let bye = read_message(&mut stdout);
    assert!(
        bye.contains(r#""id":4"#) && bye.contains(r#""result":null"#),
        "{bye}"
    );
    send(r#"{"jsonrpc":"2.0","method":"exit"}"#);
    assert!(child.wait().unwrap().success());
}

#[test]
fn exit_without_shutdown_is_a_failure_status() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_phase"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(frame(r#"{"jsonrpc":"2.0","method":"exit"}"#).as_bytes())
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(1));
}
