//! `phase lsp`: a Language Server Protocol server on stdin/stdout.
//!
//! It checks each open document on every change and publishes the same
//! diagnostics `phase check` prints -- with the same underlines and each
//! note as related information -- and lists a document's functions and
//! devices (with their registers) as symbols. Nothing is cached between
//! checks: analysis of a source file is fast enough to redo per keystroke.

use crate::diagnostics::{underline_range, Diagnostic};
use crate::json::{self, obj, Json};
use phase_ast::{Item, Span};
use phase_parser::SourceFile;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

/// Run the server until the client sends `exit`. Returns the process exit
/// status the protocol asks for: 0 after a `shutdown`, 1 otherwise.
pub fn serve() -> i32 {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut server = Server::default();
    loop {
        let Some(body) = read_message(&mut input) else {
            return 1; // the client went away without `exit`
        };
        let msg = match json::parse(&body) {
            Ok(m) => m,
            Err(e) => {
                let reply = error_response(Json::Null, -32700, &format!("parse error: {e}"));
                send(&mut out, &reply);
                continue;
            }
        };
        for reply in server.handle(&msg) {
            send(&mut out, &reply);
        }
        if let Some(code) = server.exit_code {
            return code;
        }
    }
}

/// One `Content-Length`-framed message body, or `None` at end of input.
fn read_message(input: &mut impl BufRead) -> Option<String> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0; len?];
    input.read_exact(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn send(out: &mut impl Write, msg: &Json) {
    let body = msg.to_string();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

fn response(id: Json, result: Json) -> Json {
    obj(&[("jsonrpc", "2.0".into()), ("id", id), ("result", result)])
}

fn error_response(id: Json, code: i32, message: &str) -> Json {
    obj(&[
        ("jsonrpc", "2.0".into()),
        ("id", id),
        (
            "error",
            obj(&[
                ("code", Json::Num(f64::from(code))),
                ("message", message.into()),
            ]),
        ),
    ])
}

fn notification(method: &str, params: Json) -> Json {
    obj(&[
        ("jsonrpc", "2.0".into()),
        ("method", method.into()),
        ("params", params),
    ])
}

#[derive(Default)]
struct Server {
    /// Open documents: uri -> text.
    docs: HashMap<String, String>,
    shutdown: bool,
    exit_code: Option<i32>,
}

impl Server {
    /// Handle one message; returns the messages to send back.
    fn handle(&mut self, msg: &Json) -> Vec<Json> {
        let method = msg.get("method").and_then(Json::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Json::Null);
        let uri = params
            .at(&["textDocument", "uri"])
            .and_then(Json::as_str)
            .map(str::to_string);
        match (method, id) {
            ("initialize", Some(id)) => vec![response(
                id,
                obj(&[
                    (
                        "capabilities",
                        obj(&[
                            (
                                "textDocumentSync",
                                obj(&[("openClose", true.into()), ("change", 1u32.into())]),
                            ),
                            ("documentSymbolProvider", true.into()),
                        ]),
                    ),
                    (
                        "serverInfo",
                        obj(&[
                            ("name", "phase".into()),
                            ("version", env!("CARGO_PKG_VERSION").into()),
                        ]),
                    ),
                ]),
            )],
            ("shutdown", Some(id)) => {
                self.shutdown = true;
                vec![response(id, Json::Null)]
            }
            ("exit", _) => {
                self.exit_code = Some(if self.shutdown { 0 } else { 1 });
                vec![]
            }
            ("textDocument/didOpen", None) => {
                let text = params.at(&["textDocument", "text"]).and_then(Json::as_str);
                match (uri, text) {
                    (Some(uri), Some(text)) => {
                        self.docs.insert(uri.clone(), text.to_string());
                        vec![self.publish(&uri)]
                    }
                    _ => vec![],
                }
            }
            ("textDocument/didChange", None) => {
                // Full sync: the last change holds the whole new text.
                let text = params
                    .get("contentChanges")
                    .and_then(Json::as_arr)
                    .and_then(|c| c.last())
                    .and_then(|c| c.get("text"))
                    .and_then(Json::as_str);
                match (uri, text) {
                    (Some(uri), Some(text)) => {
                        self.docs.insert(uri.clone(), text.to_string());
                        vec![self.publish(&uri)]
                    }
                    _ => vec![],
                }
            }
            ("textDocument/didClose", None) => match uri {
                Some(uri) => {
                    self.docs.remove(&uri);
                    vec![notification(
                        "textDocument/publishDiagnostics",
                        obj(&[("uri", uri.into()), ("diagnostics", Json::Arr(vec![]))]),
                    )]
                }
                None => vec![],
            },
            ("textDocument/documentSymbol", Some(id)) => {
                let symbols = uri
                    .and_then(|u| self.docs.get(&u))
                    .map(|text| document_symbols(text))
                    .unwrap_or_default();
                vec![response(id, Json::Arr(symbols))]
            }
            (_, Some(id)) => vec![error_response(
                id,
                -32601,
                &format!("method not supported: {method}"),
            )],
            (_, None) => vec![], // notifications we don't use
        }
    }

    fn publish(&self, uri: &str) -> Json {
        let (diags, files) = self.check(uri);
        let diagnostics = diags.iter().map(|d| to_lsp(d, uri, &files)).collect();
        notification(
            "textDocument/publishDiagnostics",
            obj(&[("uri", uri.into()), ("diagnostics", Json::Arr(diagnostics))]),
        )
    }

    /// Everything `phase check` would report for the document at `uri`,
    /// and the files it was loaded from. Imports are read from open
    /// documents first (unsaved edits count), then from disk.
    fn check(&self, uri: &str) -> (Vec<Diagnostic>, Vec<SourceFile>) {
        let root = uri_to_path(uri);
        let docs = &self.docs;
        let mut read = |p: &Path| -> io::Result<String> {
            if p == root {
                return Ok(docs.get(uri).cloned().unwrap_or_default());
            }
            match docs.iter().find(|(u, _)| uri_to_path(u) == p) {
                Some((_, text)) => Ok(text.clone()),
                None => std::fs::read_to_string(p),
            }
        };
        match phase_parser::load(&root, &mut read) {
            Err(e) => (vec![Diagnostic::from(&e.error)], e.files),
            Ok(loaded) => {
                let report = phase_analysis::analyze_full(&loaded.program);
                let diags = report
                    .errors
                    .iter()
                    .chain(&report.warnings)
                    .map(Diagnostic::from)
                    .collect();
                (diags, loaded.files)
            }
        }
    }
}

/// `file:///a/b%20c.phase` -> `/a/b c.phase`.
fn uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

/// `/a/b c.phase` -> `file:///a/b%20c.phase`.
fn path_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = String::from("file://");
    for b in abs.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The number of UTF-16 code units in the first `chars` chars of `line`.
fn utf16_col(line: &str, chars: usize) -> u32 {
    line.chars().take(chars).map(|c| c.len_utf16() as u32).sum()
}

/// The LSP range to underline for `span` -- the same columns `phase check`
/// underlines, narrowed to `subject` when it occurs there.
fn range(span: Span, text: &str, subject: Option<&str>) -> Json {
    let line_no = span.line.max(1) - 1;
    let line = text.lines().nth(line_no as usize).unwrap_or("");
    let (start, end) = underline_range(span, line, subject);
    let pos = |col: usize| {
        obj(&[
            ("line", line_no.into()),
            ("character", utf16_col(line, col.saturating_sub(1)).into()),
        ])
    };
    obj(&[("start", pos(start)), ("end", pos(end))])
}

/// One diagnostic for the document at `uri` (file 0 of `files`). A
/// problem located in an imported file is shown on the `import` that
/// brought that file in, with a link to where it really is.
fn to_lsp(d: &Diagnostic, uri: &str, files: &[SourceFile]) -> Json {
    let text_of = |file: u32| files.get(file as usize).map_or("", |f| f.src.as_str());
    let uri_of = |file: u32| match file {
        0 => uri.to_string(),
        n => files
            .get(n as usize)
            .map_or_else(|| uri.to_string(), |f| path_to_uri(&f.path)),
    };
    let location = |s: Span, subject: Option<&str>| {
        obj(&[
            ("uri", uri_of(s.file).into()),
            ("range", range(s, text_of(s.file), subject)),
        ])
    };
    // A diagnostic without a location (rare) goes on the first line.
    let span = d.span.unwrap_or(Span {
        line: 1,
        col: 1,
        end_line: 1,
        end_col: 1,
        file: 0,
    });
    let mut related: Vec<Json> = Vec::new();
    let (shown_at, message) = match files.get(span.file as usize) {
        Some(f) if span.file != 0 => {
            related.push(obj(&[
                ("location", location(span, d.subject.as_deref())),
                ("message", d.message.clone().into()),
            ]));
            let name = f.path.file_name().map_or_else(
                || f.path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let at = f.via.unwrap_or(Span {
                line: 1,
                col: 1,
                end_line: 1,
                end_col: 1,
                file: 0,
            });
            (
                range(at, text_of(0), None),
                format!("in {name}:{}: {}", span.line, d.message),
            )
        }
        _ => (
            range(span, text_of(0), d.subject.as_deref()),
            d.message.clone(),
        ),
    };
    related.extend(d.notes.iter().map(|(s, m)| {
        obj(&[
            ("location", location(*s, None)),
            ("message", m.clone().into()),
        ])
    }));
    let mut fields = vec![
        ("range", shown_at),
        (
            "severity",
            (if d.severity == "warning" { 2u32 } else { 1u32 }).into(),
        ),
        ("source", "phase".into()),
        ("message", message.into()),
    ];
    if !related.is_empty() {
        fields.push(("relatedInformation", Json::Arr(related)));
    }
    obj(&fields)
}

/// Functions, extern functions and devices (with their registers), for
/// the editor's outline. Entities and typestate machines carry no spans in
/// the AST yet, so they are not listed.
fn document_symbols(text: &str) -> Vec<Json> {
    let Ok(program) = phase_parser::parse(text) else {
        return Vec::new();
    };
    // LSP SymbolKind: 12 Function, 2 Module, 8 Field.
    let symbol = |name: &str, kind: u32, span: Span, children: Vec<Json>| {
        let r = range(span, text, Some(name));
        obj(&[
            ("name", name.into()),
            ("kind", kind.into()),
            ("range", r.clone()),
            ("selectionRange", r),
            ("children", Json::Arr(children)),
        ])
    };
    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(f) => Some(symbol(&f.name, 12, f.name_span, vec![])),
            Item::ExternFn(f) => Some(symbol(&f.name, 12, f.name_span, vec![])),
            Item::Device(d) => Some(symbol(
                &d.name,
                2,
                d.span,
                d.regs
                    .iter()
                    .map(|r| symbol(&r.name, 8, r.span, vec![]))
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_columns_count_surrogate_pairs() {
        assert_eq!(utf16_col("a😀b", 2), 3);
        assert_eq!(utf16_col("é", 1), 1);
    }

    #[test]
    fn a_rejected_program_gets_a_diagnostic_narrowed_to_its_subject() {
        let src = "extern fn look(x: read buffer<u8, 4> @RAM);\nfn main() {\n    buffer<u8, 4> b @DMA;\n    look(b);\n    sync(b);\n}\n";
        let mut server = Server::default();
        server.docs.insert("file:///t.phase".into(), src.into());
        let (diags, files) = server.check("file:///t.phase");
        assert_eq!(diags.len(), 1);
        let lsp = to_lsp(&diags[0], "file:///t.phase", &files);
        assert_eq!(lsp.at(&["range", "start", "line"]), Some(&Json::Num(3.0)));
        // `look(b)` starts at character 4; the subject `b` is at 9.
        assert_eq!(
            lsp.at(&["range", "start", "character"]),
            Some(&Json::Num(9.0))
        );
        assert_eq!(lsp.get("severity"), Some(&Json::Num(1.0)));
        assert!(lsp.get("relatedInformation").is_some());
    }

    #[test]
    fn uris_and_paths_round_trip() {
        let p = Path::new("/home/me/my code/a+b.phase");
        assert_eq!(path_to_uri(p), "file:///home/me/my%20code/a%2Bb.phase");
        assert_eq!(uri_to_path(&path_to_uri(p)), p);
    }

    #[test]
    fn an_error_in_an_imported_file_is_shown_on_the_import() {
        let dir = std::env::temp_dir().join("phase_lsp_import_test");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        // The library itself is broken: it reads a buffer the DMA engine owns.
        std::fs::write(
            dir.join("lib/helpers.phase"),
            "extern fn look(x: read buffer<u8, 4> @RAM);\nfn peek(b: buffer<u8, 4> @DMA) {\n    look(b);\n}\n",
        )
        .unwrap();
        let root = dir.join("app.phase");
        let uri = path_to_uri(&root);
        let mut server = Server::default();
        // Unsaved text of the open document; the library comes from disk.
        server.docs.insert(
            uri.clone(),
            "import \"lib/helpers.phase\";\nfn main() {\n}\n".into(),
        );
        let (diags, files) = server.check(&uri);
        assert_eq!(files.len(), 2);
        assert_eq!(diags.len(), 1, "{diags:?}");
        let lsp = to_lsp(&diags[0], &uri, &files);
        // Shown on line 0 (the import) of the open document...
        assert_eq!(lsp.at(&["range", "start", "line"]), Some(&Json::Num(0.0)));
        let msg = lsp.get("message").and_then(Json::as_str).unwrap();
        assert!(
            msg.starts_with("in helpers.phase:3: entity 'b' is in domain @DMA"),
            "{msg}"
        );
        // ...linked to where it really is.
        let rel = lsp
            .get("relatedInformation")
            .and_then(Json::as_arr)
            .unwrap();
        let first = rel[0]
            .at(&["location", "uri"])
            .and_then(Json::as_str)
            .unwrap();
        assert!(first.ends_with("/lib/helpers.phase"), "{first}");
        assert_eq!(
            rel[0].at(&["location", "range", "start", "line"]),
            Some(&Json::Num(2.0))
        );
    }
}
