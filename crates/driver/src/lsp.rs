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
use std::collections::HashMap;
use std::io::{self, BufRead, Write};

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
        let text = self.docs.get(uri).map(String::as_str).unwrap_or("");
        let diagnostics = check(text).iter().map(|d| to_lsp(d, uri, text)).collect();
        notification(
            "textDocument/publishDiagnostics",
            obj(&[("uri", uri.into()), ("diagnostics", Json::Arr(diagnostics))]),
        )
    }
}

/// Everything `phase check` would report for `text`.
fn check(text: &str) -> Vec<Diagnostic> {
    match phase_parser::parse(text) {
        Err(e) => vec![Diagnostic::from(&e)],
        Ok(program) => {
            let report = phase_analysis::analyze_full(&program);
            report
                .errors
                .iter()
                .chain(&report.warnings)
                .map(Diagnostic::from)
                .collect()
        }
    }
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

fn to_lsp(d: &Diagnostic, uri: &str, text: &str) -> Json {
    // A diagnostic without a location (rare) goes on the first line.
    let span = d.span.unwrap_or(Span {
        line: 1,
        col: 1,
        end_line: 1,
        end_col: 1,
    });
    let related: Vec<Json> = d
        .notes
        .iter()
        .map(|(s, m)| {
            obj(&[
                (
                    "location",
                    obj(&[("uri", uri.into()), ("range", range(*s, text, None))]),
                ),
                ("message", m.clone().into()),
            ])
        })
        .collect();
    let mut fields = vec![
        ("range", range(span, text, d.subject.as_deref())),
        (
            "severity",
            (if d.severity == "warning" { 2u32 } else { 1u32 }).into(),
        ),
        ("source", "phase".into()),
        ("message", d.message.clone().into()),
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
        let diags = check(src);
        assert_eq!(diags.len(), 1);
        let lsp = to_lsp(&diags[0], "file:///t.phase", src);
        assert_eq!(lsp.at(&["range", "start", "line"]), Some(&Json::Num(3.0)));
        // `look(b)` starts at character 4; the subject `b` is at 9.
        assert_eq!(
            lsp.at(&["range", "start", "character"]),
            Some(&Json::Num(9.0))
        );
        assert_eq!(lsp.get("severity"), Some(&Json::Num(1.0)));
        assert!(lsp.get("relatedInformation").is_some());
    }
}
