//! `phase lsp`: a Language Server Protocol server on stdin/stdout.
//!
//! It checks each open document on every change and publishes the same
//! diagnostics `phase check` prints -- with the same underlines and each
//! note as related information -- and lists a document's functions and
//! devices (with their registers) as symbols. Hovering a buffer or other
//! local shows who owns it at that line and how it got there; hovering a
//! function, device, register or event shows its declaration, and
//! go-to-definition jumps to it, across imports. Nothing is cached between
//! requests: analysis of a source file is fast enough to redo each time.

use crate::diagnostics::{underline_range, Diagnostic};
use crate::json::{self, obj, Json};
use phase_ast::{device_regs, Item, Program, Span};
use phase_parser::{Loaded, SourceFile};
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
                            ("hoverProvider", true.into()),
                            ("definitionProvider", true.into()),
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
            ("textDocument/hover", Some(id)) => {
                let hover = match (uri, position(&params)) {
                    (Some(uri), Some(pos)) => self.hover(&uri, pos),
                    _ => None,
                };
                let result = hover.map_or(Json::Null, |md| {
                    obj(&[(
                        "contents",
                        obj(&[("kind", "markdown".into()), ("value", md.into())]),
                    )])
                });
                vec![response(id, result)]
            }
            ("textDocument/definition", Some(id)) => {
                let def = match (uri, position(&params)) {
                    (Some(uri), Some(pos)) => self.definition(&uri, pos),
                    _ => None,
                };
                vec![response(id, def.unwrap_or(Json::Null))]
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

    /// Load the document at `uri` and its imports. Imports are read from
    /// open documents first (unsaved edits count), then from disk.
    fn load(&self, uri: &str) -> Result<Loaded, phase_parser::LoadError> {
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
        phase_parser::load(&root, &mut read)
    }

    /// Everything `phase check` would report for the document at `uri`,
    /// and the files it was loaded from.
    fn check(&self, uri: &str) -> (Vec<Diagnostic>, Vec<SourceFile>) {
        match self.load(uri) {
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

impl Server {
    /// Markdown for what is under `pos` in the document at `uri`.
    fn hover(&self, uri: &str, pos: (u32, u32)) -> Option<String> {
        let loaded = self.load(uri).ok()?;
        let text = &loaded.files[0].src;
        let (chain, idx) = word_chain_at(text, pos)?;
        let word = chain[idx].as_str();
        let line = pos.0 + 1;
        // A local of the function the cursor is in.
        if idx == 0 {
            if let Some(f) = enclosing_fn(&loaded.program, line) {
                if let Some(md) = local_hover(&loaded.program, &f.name, word, line, text) {
                    return Some(md);
                }
            }
        }
        let (span, kind) = find_decl(&loaded.program, &chain[..=idx])?;
        let src = &loaded.files.get(span.file as usize)?.src;
        let mut md = format!("```phase\n{}\n```", decl_text(src, span));
        if let Some(extra) = kind_note(&loaded.program, &kind, word) {
            md.push_str(&format!("\n\n{extra}"));
        }
        Some(md)
    }

    /// Where what is under `pos` is declared.
    fn definition(&self, uri: &str, pos: (u32, u32)) -> Option<Json> {
        let loaded = self.load(uri).ok()?;
        let text = &loaded.files[0].src;
        let (chain, idx) = word_chain_at(text, pos)?;
        let word = chain[idx].as_str();
        let line = pos.0 + 1;
        let span = if idx == 0 {
            enclosing_fn(&loaded.program, line)
                .and_then(|f| local_decl(&loaded.program, f, word, text))
        } else {
            None
        };
        let span = match span {
            Some(s) => s,
            None => find_decl(&loaded.program, &chain[..=idx])?.0,
        };
        let file = loaded.files.get(span.file as usize)?;
        let target_uri = if span.file == 0 {
            uri.to_string()
        } else {
            path_to_uri(&file.path)
        };
        Some(obj(&[
            ("uri", target_uri.into()),
            ("range", word_range(span, &file.src, word)),
        ]))
    }
}

/// The range of `word` on `span`'s line, at or after its start (a
/// declaration's span can start before its name), else `span`'s own.
fn word_range(span: Span, src: &str, word: &str) -> Json {
    let line_no = span.line.max(1) - 1;
    let line = src.lines().nth(line_no as usize).unwrap_or("");
    let chars: Vec<char> = line.chars().collect();
    let w: Vec<char> = word.chars().collect();
    let from = (span.col.max(1) - 1) as usize;
    for i in from..chars.len().saturating_sub(w.len() - 1) {
        let whole = chars[i..i + w.len()] == w[..]
            && (i == 0 || !is_word(chars[i - 1]))
            && chars.get(i + w.len()).is_none_or(|c| !is_word(*c));
        if whole {
            let pos = |c: usize| {
                obj(&[
                    ("line", line_no.into()),
                    ("character", utf16_col(line, c).into()),
                ])
            };
            return obj(&[("start", pos(i)), ("end", pos(i + w.len()))]);
        }
    }
    range(span, src, None)
}

/// `textDocument/*` request position: (line, character), both 0-based.
fn position(params: &Json) -> Option<(u32, u32)> {
    let num = |k: &str| match params.at(&["position", k]) {
        Some(Json::Num(n)) => Some(*n as u32),
        _ => None,
    };
    Some((num("line")?, num("character")?))
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The dotted name under `pos` (`Fz.CTRL.EN` -> [Fz, CTRL, EN]) and which
/// part of it the cursor is on.
fn word_chain_at(text: &str, pos: (u32, u32)) -> Option<(Vec<String>, usize)> {
    let line: Vec<char> = text.lines().nth(pos.0 as usize)?.chars().collect();
    // The UTF-16 column -> a char index.
    let mut units = 0u32;
    let mut at = line.len();
    for (i, c) in line.iter().enumerate() {
        if units >= pos.1 {
            at = i;
            break;
        }
        units += c.len_utf16() as u32;
    }
    if at == line.len() || !is_word(line[at]) {
        // Just past the end of a word counts too.
        if at > 0 && is_word(line[at - 1]) {
            at -= 1;
        } else {
            return None;
        }
    }
    let mut start = at;
    loop {
        if start > 0 && (is_word(line[start - 1]) || line[start - 1] == '.') {
            start -= 1;
        } else if start > 0 && line[start - 1] == ']' {
            // v2.4: skip a register array's index, `CR[3]`.
            match line[..start].iter().rposition(|c| *c == '[') {
                Some(open) => start = open,
                None => break,
            }
        } else {
            break;
        }
    }
    let mut end = at;
    while end < line.len() && is_word(line[end]) {
        end += 1;
    }
    let before: String = line[start..at].iter().collect();
    let chain_text: String = line[start..end].iter().collect();
    // Drop the indexes: `Dma.CR[3].EN` -> Dma, CR, EN.
    let mut plain = String::new();
    let mut depth = 0;
    for c in chain_text.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let before: String = {
        let mut out = String::new();
        let mut depth = 0;
        for c in before.chars() {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ if depth == 0 => out.push(c),
                _ => {}
            }
        }
        out
    };
    let chain: Vec<String> = plain
        .split('.')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let idx = before
        .matches('.')
        .count()
        .min(chain.len().saturating_sub(1));
    if chain.is_empty() || chain[idx].starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some((chain, idx))
}

/// The function of file 0 whose text contains `line` (1-based).
fn enclosing_fn(program: &Program, line: u32) -> Option<&phase_ast::FnDecl> {
    program.items.iter().find_map(|i| match i {
        Item::Fn(f)
            if f.name_span.file == 0 && f.name_span.line <= line && line <= f.body.close.line =>
        {
            Some(f)
        }
        _ => None,
    })
}

/// What a declaration is, for the extra line in its hover.
enum DeclKind {
    Fn,
    Device,
    Register,
    Field,
    Event,
    Const,
    Other,
}

/// The declaration a (dotted) name refers to.
fn find_decl(program: &Program, chain: &[String]) -> Option<(Span, DeclKind)> {
    let first = chain.first()?.as_str();
    for item in &program.items {
        match item {
            Item::Fn(f) if chain.len() == 1 && f.name == first => {
                return Some((f.name_span, DeclKind::Fn))
            }
            Item::ExternFn(f) if chain.len() == 1 && f.name == first => {
                return Some((f.name_span, DeclKind::Fn))
            }
            Item::Event(e) if chain.len() == 1 && e.name == first => {
                return Some((e.span, DeclKind::Event))
            }
            Item::Const(c) if chain.len() == 1 && c.name == first => {
                return Some((c.span, DeclKind::Const))
            }
            Item::DeviceType(t) if chain.len() == 1 && t.name == first => {
                return Some((t.span, DeclKind::Other))
            }
            Item::Device(d) if d.name == first => {
                if chain.len() == 1 {
                    return Some((d.span, DeclKind::Device));
                }
                let reg = device_regs(program, d)?
                    .iter()
                    .find(|r| r.name == chain[1])?;
                if chain.len() == 2 {
                    return Some((reg.span, DeclKind::Register));
                }
                let field = reg.fields.iter().find(|f| f.name == chain[2])?;
                return Some((field.span, DeclKind::Field));
            }
            _ => {}
        }
    }
    None
}

/// The source of a declaration: its first line, continued while brackets
/// are open (a multi-line `extern fn`), without a body.
fn decl_text(src: &str, span: Span) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut depth = 0i32;
    for l in lines.iter().skip(span.line.max(1) as usize - 1).take(8) {
        let mut t = l.trim().to_string();
        if let Some(i) = t.find('{') {
            if depth == 0 && !t.contains('}') {
                t = t[..i].trim_end().to_string();
                out.push(t);
                break;
            }
        }
        depth += t.matches('(').count() as i32 - t.matches(')').count() as i32;
        out.push(t);
        if depth <= 0 {
            break;
        }
    }
    out.join("\n    ")
}

/// One more line for some declarations: who signals and waits for an
/// event, what a device's registers are.
fn kind_note(program: &Program, kind: &DeclKind, name: &str) -> Option<String> {
    match kind {
        DeclKind::Event => {
            let mut signal = Vec::new();
            let mut wait = Vec::new();
            for item in &program.items {
                if let Item::Fn(f) = item {
                    let (s, w) = event_uses(&f.body, name);
                    if s {
                        signal.push(format!("`{}`", f.name));
                    }
                    if w {
                        wait.push(format!("`{}`", f.name));
                    }
                }
            }
            let list = |v: &[String]| {
                if v.is_empty() {
                    "nothing".to_string()
                } else {
                    v.join(", ")
                }
            };
            Some(format!(
                "Signalled in {}; waited for in {}.",
                list(&signal),
                list(&wait)
            ))
        }
        DeclKind::Device => {
            let d = program.items.iter().find_map(|i| match i {
                Item::Device(d) if d.name == name => Some(d),
                _ => None,
            })?;
            let regs = device_regs(program, d)?;
            let names: Vec<String> = regs.iter().map(|r| format!("`{}`", r.name)).collect();
            Some(format!("Registers: {}.", names.join(", ")))
        }
        DeclKind::Const => {
            let v = const_value(program, name)?;
            Some(format!("= {v} ({v:#x})"))
        }
        DeclKind::Fn | DeclKind::Register | DeclKind::Field | DeclKind::Other => None,
    }
}

/// A constant's value, following the constants it is computed from.
fn const_value(program: &Program, name: &str) -> Option<u64> {
    fn eval(program: &Program, name: &str, depth: usize) -> Option<u64> {
        if depth > 64 {
            return None; // a cycle (reported by the analyzer)
        }
        let c = program.items.iter().find_map(|i| match i {
            Item::Const(c) if c.name == name => Some(c),
            _ => None,
        })?;
        let lookup = |n: &str| eval(program, n, depth + 1).map(Ok);
        phase_ast::eval_const(&c.value, &lookup).ok().flatten()
    }
    eval(program, name, 0)
}

/// Whether `block` signals / waits for `event`.
fn event_uses(block: &phase_ast::Block, event: &str) -> (bool, bool) {
    use phase_ast::Stmt;
    let (mut s, mut w) = (false, false);
    for st in &block.stmts {
        let (a, b) = match &st.node {
            Stmt::Signal { name } => (name == event, false),
            Stmt::Wait { name, .. } => (false, name == event),
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                let (a1, b1) = event_uses(then_block, event);
                let (a2, b2) = else_block
                    .as_ref()
                    .map_or((false, false), |e| event_uses(e, event));
                (a1 || a2, b1 || b2)
            }
            Stmt::While { body, .. } | Stmt::Critical { body } => event_uses(body, event),
            _ => (false, false),
        };
        s |= a;
        w |= b;
    }
    (s, w)
}

fn span_contains(s: Span, line: u32) -> bool {
    s.line <= line && line <= s.end_line.max(s.line)
}

/// A local's hover: what it is at `line`, and the statements so far in
/// this function that changed it.
fn local_hover(
    program: &Program,
    fn_name: &str,
    name: &str,
    line: u32,
    text: &str,
) -> Option<String> {
    let (_, steps) = phase_analysis::trace(program);
    let steps: Vec<&phase_analysis::TraceStep> = steps
        .iter()
        .filter(|s| s.fn_name == fn_name && s.span.file == 0)
        .collect();
    let state = |v: &[(String, String)]| v.iter().find(|(n, _)| n == name).map(|(_, d)| d.clone());
    // The innermost statement on this line: the state as it starts (or,
    // for the line declaring the name, as it ends).
    let here: Vec<&&phase_analysis::TraceStep> = steps
        .iter()
        .filter(|s| span_contains(s.span, line))
        .collect();
    let innermost = here
        .iter()
        .filter(|s| !s.compound)
        .map(|s| s.span)
        .min_by_key(|s| (s.end_line - s.line, std::cmp::Reverse(s.col)))
        .or_else(|| here.iter().map(|s| s.span).max_by_key(|s| (s.line, s.col)));
    let mut now: Vec<String> = Vec::new();
    match innermost {
        Some(sp) => {
            for s in steps.iter().filter(|s| s.span == sp) {
                if let Some(d) = state(&s.before).or_else(|| state(&s.after)) {
                    if !now.contains(&d) {
                        now.push(d);
                    }
                }
            }
        }
        None => {
            // Not on a statement (a parameter in the header, say): the
            // state as the function starts, or as it ends.
            let first = steps.iter().min_by_key(|s| (s.span.line, s.span.col));
            let last = steps
                .iter()
                .filter(|s| s.span.line <= line)
                .max_by_key(|s| (s.span.line, s.span.col));
            let pick = if steps.iter().all(|s| s.span.line > line) {
                first.and_then(|s| state(&s.before))
            } else {
                last.and_then(|s| state(&s.after))
            };
            now.extend(pick);
        }
    }
    if now.is_empty() {
        return None;
    }
    // Statements up to here that changed it, in source order.
    let mut changes: Vec<(Span, String)> = Vec::new();
    let mut seen: Vec<Span> = Vec::new();
    let mut ordered: Vec<&&phase_analysis::TraceStep> = steps
        .iter()
        .filter(|s| !s.compound && s.span.line < line)
        .collect();
    ordered.sort_by_key(|s| (s.span.line, s.span.col));
    for s in ordered {
        let (b, a) = (state(&s.before), state(&s.after));
        if let Some(after) = a.filter(|a| b.as_ref() != Some(a)) {
            if !seen.contains(&s.span) {
                seen.push(s.span);
                // Where it was introduced: its state then is in the line
                // itself (and on the first line of the hover, if unchanged).
                let what = if b.is_none() {
                    "declared".to_string()
                } else {
                    after
                };
                changes.push((s.span, what));
            }
        }
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut md = format!("**{name}**: {}", now.join(" *or* "));
    if !changes.is_empty() {
        md.push_str("\n\n");
        let skip = changes.len().saturating_sub(8);
        if skip > 0 {
            md.push_str(&format!("({skip} earlier changes)\n\n"));
        }
        for (sp, desc) in &changes[skip..] {
            let src = lines.get(sp.line as usize - 1).map_or("", |l| l.trim());
            if desc == "declared" {
                md.push_str(&format!("- line {}: `{src}` — declared\n", sp.line));
            } else {
                md.push_str(&format!("- line {}: `{src}` → {desc}\n", sp.line));
            }
        }
    }
    Some(md)
}

/// Where a local of `f` is declared: a parameter in the header, or the
/// statement that first introduces it.
fn local_decl(program: &Program, f: &phase_ast::FnDecl, name: &str, text: &str) -> Option<Span> {
    if f.params.iter().any(|p| p.name == name) {
        // Parameters carry no span: find `name:` in the header.
        let lines: Vec<&str> = text.lines().collect();
        for ln in f.name_span.line..=f.body.close.line {
            let l = lines.get(ln as usize - 1)?;
            let mut from = 0;
            while let Some(i) = l[from..].find(name) {
                let at = from + i;
                let before_ok = at == 0 || !is_word(l[..at].chars().next_back().unwrap());
                let rest = l[at + name.len()..].trim_start();
                if before_ok && rest.starts_with(':') {
                    let col = l[..at].chars().count() as u32 + 1;
                    return Some(Span {
                        line: ln,
                        col,
                        end_line: ln,
                        end_col: col + name.chars().count() as u32,
                        file: 0,
                    });
                }
                from = at + name.len();
            }
            if l.contains('{') {
                break;
            }
        }
        return None;
    }
    let (_, steps) = phase_analysis::trace(program);
    steps
        .iter()
        .filter(|s| s.fn_name == f.name && s.span.file == 0)
        .filter(|s| {
            !s.before.iter().any(|(n, _)| n == name) && s.after.iter().any(|(n, _)| n == name)
        })
        .filter(|s| !s.compound)
        .map(|s| s.span)
        .min_by_key(|s| (s.line, s.col))
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

    const HOVER_SRC: &str = "device Fz @ 0x40000000 {
    CTRL: read write u32 @ 0x00 { EN: bit 0 }
}
event done;
extern fn start(x: write buffer<u8, 8> @RAM -> @DMA);
extern fn look(x: read buffer<u8, 8> @RAM);
interrupt fn on_dma() @ irq 3 {
    signal done;
}
fn main() {
    buffer<u8, 8> b @RAM;
    start(b);
    volatile_write(Fz.CTRL.EN, 1);
    wait done for b;
    look(b);
}
";

    fn hover_at(line: u32, character: u32) -> Option<String> {
        let mut server = Server::default();
        server
            .docs
            .insert("file:///h.phase".into(), HOVER_SRC.into());
        server.hover("file:///h.phase", (line, character))
    }

    #[test]
    fn hovering_a_buffer_shows_its_owner_there_and_how_it_got_there() {
        // `start(b);` (line 12): b is still the CPU's as it starts.
        let md = hover_at(11, 10).unwrap();
        assert!(md.starts_with("**b**: `buffer<u8, 8>`, in @RAM"), "{md}");
        // `look(b);` (line 15): handed back by the interrupt.
        let md = hover_at(14, 9).unwrap();
        assert!(md.starts_with("**b**: `buffer<u8, 8>`, in @RAM"), "{md}");
        assert!(
            md.contains(
                "- line 12: `start(b);` → `buffer<u8, 8>`, in @DMA (the DMA engine has it)"
            ),
            "{md}"
        );
        assert!(
            md.contains("- line 14: `wait done for b;` → `buffer<u8, 8>`, in @RAM"),
            "{md}"
        );
        assert!(
            md.contains("- line 11: `buffer<u8, 8> b @RAM;` — declared"),
            "{md}"
        );
        // Between the two, on `wait done for b;`: the engine has it.
        let md = hover_at(13, 18).unwrap();
        assert!(md.contains("in @DMA"), "{md}");
    }

    #[test]
    fn hovering_declarations_shows_them() {
        // `start` in `start(b);`
        let md = hover_at(11, 6).unwrap();
        assert!(
            md.contains("extern fn start(x: write buffer<u8, 8> @RAM -> @DMA);"),
            "{md}"
        );
        // `EN` in `Fz.CTRL.EN`, and `CTRL`.
        let md = hover_at(12, 27).unwrap();
        assert!(
            md.contains("CTRL: read write u32 @ 0x00 { EN: bit 0 }"),
            "{md}"
        );
        let md = hover_at(12, 23).unwrap();
        assert!(md.contains("CTRL: read write u32"), "{md}");
        // The event: who signals it, who waits.
        let md = hover_at(13, 10).unwrap();
        assert!(
            md.contains("Signalled in `on_dma`; waited for in `main`."),
            "{md}"
        );
        assert_eq!(hover_at(9, 0), None); // `fn`
                                          // A constant: its declaration and its value.
        let mut server = Server::default();
        server.docs.insert(
            "file:///c.phase".into(),
            "const A: u32 = 1 << 4;\nconst B: u32 = A | 1;\nfn main() {\n}\n".into(),
        );
        let md = server.hover("file:///c.phase", (1, 6)).unwrap();
        assert!(md.contains("const B: u32 = A | 1;"), "{md}");
        assert!(md.contains("= 17 (0x11)"), "{md}");
        // A field of a register array element.
        server.docs.insert(
            "file:///a.phase".into(),
            "device D @ 0x40000000 {\n    CR[4]: read write u32 @ 0x0 step 8 { EN: bit 0 }\n}\nfn main() {\n    volatile_write(D.CR[2].EN, 1);\n}\n".into(),
        );
        let md = server.hover("file:///a.phase", (4, 22)).unwrap(); // CR
        assert!(md.contains("CR[4]: read write u32"), "{md}");
        let md = server.hover("file:///a.phase", (4, 27)).unwrap(); // EN
        assert!(
            md.contains("CR[4]: read write u32 @ 0x0 step 8 { EN: bit 0 }"),
            "{md}"
        );
    }

    #[test]
    fn go_to_definition_finds_locals_registers_and_imports() {
        let mut server = Server::default();
        server
            .docs
            .insert("file:///h.phase".into(), HOVER_SRC.into());
        let def = |line, ch| server.definition("file:///h.phase", (line, ch)).unwrap();
        // `b` in `look(b)` -> its declaration on line 11 (0-based 10).
        let d = def(14, 9);
        assert_eq!(d.at(&["range", "start", "line"]), Some(&Json::Num(10.0)));
        assert_eq!(
            d.at(&["range", "start", "character"]),
            Some(&Json::Num(18.0))
        );
        // `EN` -> the field.
        let d = def(12, 27);
        assert_eq!(d.at(&["range", "start", "line"]), Some(&Json::Num(1.0)));
        assert_eq!(
            d.at(&["range", "start", "character"]),
            Some(&Json::Num(34.0))
        );

        // A device from an imported file.
        let dir = std::env::temp_dir().join("phase_lsp_def_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("board.phase"),
            "// board\ndevice Uart0 @ 0x40004000 {\n    DATA: read write u32 @ 0x00;\n}\n",
        )
        .unwrap();
        let root = dir.join("app.phase");
        let uri = path_to_uri(&root);
        server.docs.insert(
            uri.clone(),
            "import \"board.phase\";\nfn main() {\n    volatile_write(Uart0.DATA, 65);\n}\n".into(),
        );
        let d = server.definition(&uri, (2, 26)).unwrap();
        let target = d.get("uri").and_then(Json::as_str).unwrap();
        assert!(target.ends_with("/board.phase"), "{target}");
        assert_eq!(d.at(&["range", "start", "line"]), Some(&Json::Num(2.0)));
    }
}
