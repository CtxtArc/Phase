//! `phase import-svd` (v3.0): a PHASE board file from a CMSIS-SVD file,
//! the register description every Cortex-M vendor publishes.
//!
//! What SVD says, the import keeps: peripherals and their base addresses,
//! registers (offset, size, access, reset value), fields, and the
//! interrupt numbers (as `const NAME_IRQn`). A peripheral `derivedFrom`
//! another shares its layout, so two or more chosen peripherals with one
//! layout become a `device type` and its instances. SVD arrays (`dim`) and
//! clusters become register arrays; so do registers a vendor wrote out one
//! by one (`S0CR` .. `S7CR`, evenly spaced and laid out alike), which
//! become `SxCR[8]`. Fields marked `oneToClear`, `zeroToClear` or read
//! `clear` become `w1c`, `rc_w0` and `rc`.
//!
//! What SVD doesn't say, the import can't: which registers hold DMA
//! addresses (`address`, `until`), and -- for vendors who leave
//! `modifiedWriteValues` out, as ST does -- which flags clear on write.
//! Those are added by hand; the file says so at the top. Anything PHASE
//! can't express (a second register at the same offset, overlapping
//! fields, a 64-bit register) is left out and reported.

pub mod xml;

use std::collections::{BTreeMap, HashMap, HashSet};
use xml::Element;

/// What to import.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Peripherals to import, by name (any case); `None`: all of them.
    pub only: Option<Vec<String>>,
}

/// The board file, and what was left out of it.
#[derive(Debug, Clone)]
pub struct Imported {
    pub text: String,
    pub notes: Vec<String>,
    pub devices: usize,
    pub registers: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Plain,
    W1c,
    Rc,
    RcW0,
}

#[derive(Debug, Clone, PartialEq)]
struct Field {
    name: String,
    lo: u32,
    hi: u32,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Access {
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq)]
struct Reg {
    name: String,
    description: String,
    offset: u64,
    bits: u32,
    access: Access,
    reset: Option<u64>,
    fields: Vec<Field>,
    /// `(count, step)`.
    array: Option<(u32, u64)>,
    /// `S0CR..S7CR`, for a folded array.
    folded_from: Option<String>,
}

#[derive(Debug, Clone)]
struct Periph {
    name: String,
    derived_from: Option<String>,
    description: String,
    group: String,
    base: u64,
    regs: Vec<Reg>,
}

/// Register properties that SVD lets a device, peripheral or cluster set
/// for everything inside it.
#[derive(Debug, Clone, Copy)]
struct Defaults {
    bits: u32,
    access: Access,
    reset: Option<u64>,
}

impl Defaults {
    fn within(self, e: &Element, notes: &mut Vec<String>, at: &str) -> Defaults {
        Defaults {
            bits: e
                .text_of("size")
                .and_then(parse_num)
                .map_or(self.bits, |v| v as u32),
            access: e
                .text_of("access")
                .map_or(self.access, |a| parse_access(a, notes, at)),
            reset: e.text_of("resetValue").and_then(parse_num).or(self.reset),
        }
    }
}

/// An SVD number: `0x1F`, `0b101`, `#101` (binary) or decimal.
fn parse_num(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(b) = s
        .strip_prefix("0b")
        .or_else(|| s.strip_prefix("0B"))
        .or_else(|| s.strip_prefix('#'))
    {
        u64::from_str_radix(b, 2).ok()
    } else {
        s.parse().ok()
    }
}

fn parse_access(a: &str, notes: &mut Vec<String>, at: &str) -> Access {
    match a.trim() {
        "read-only" => Access::Read,
        "write-only" | "writeOnce" => Access::Write,
        "read-write" | "read-writeOnce" => Access::ReadWrite,
        other => {
            notes.push(format!(
                "{at}: unknown access '{other}', taken as read-write"
            ));
            Access::ReadWrite
        }
    }
}

const KEYWORDS: &[&str] = &[
    "entity",
    "fn",
    "let",
    "move",
    "sync",
    "borrow",
    "release",
    "destroy",
    "swap",
    "rotate",
    "split",
    "join",
    "into",
    "state",
    "if",
    "else",
    "while",
    "return",
    "extern",
    "read",
    "write",
    "buffer",
    "true",
    "false",
    "device",
    "type",
    "interrupt",
    "event",
    "signal",
    "wait",
    "for",
    "critical",
    "import",
    "const",
    "bit",
    "bits",
    "step",
    "address",
    "until",
    "w1c",
    "rc",
    "rc_w0",
    "volatile_read",
    "volatile_write",
    "address_of",
];

/// A PHASE identifier for an SVD name.
fn ident(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if KEYWORDS.contains(&s.as_str()) {
        s.push('_');
    }
    s
}

/// One line of a description, for a comment.
fn one_line(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut line = words.join(" ");
    if line.len() > 100 {
        let mut cut = 97;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push_str("...");
    }
    line
}

/// SVD's `dim` / `dimIncrement` / `dimIndex`, if present.
struct Dim {
    count: u32,
    step: u64,
    /// The names `%s` stands for.
    index: Vec<String>,
}

fn dim_of(e: &Element) -> Option<Dim> {
    let count = e.text_of("dim").and_then(parse_num)? as u32;
    let step = e.text_of("dimIncrement").and_then(parse_num).unwrap_or(0);
    let index = match e.text_of("dimIndex") {
        None => (0..count).map(|i| i.to_string()).collect(),
        Some(list) if list.contains('-') && !list.contains(',') => {
            let (a, b) = list.split_once('-').unwrap_or((list, list));
            match (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                (Ok(a), Ok(b)) if b >= a => (a..=b).map(|i| i.to_string()).collect(),
                _ => list.split('-').map(|x| x.trim().to_string()).collect(),
            }
        }
        Some(list) => list.split(',').map(|x| x.trim().to_string()).collect(),
    };
    Some(Dim { count, step, index })
}

/// `0, 1, 2 ..`: an index list a PHASE array can stand for.
fn is_zero_based(index: &[String]) -> bool {
    index.iter().enumerate().all(|(i, x)| *x == i.to_string())
}

fn read_fields(r: &Element, bits: u32, at: &str, notes: &mut Vec<String>) -> Vec<Field> {
    let mut out: Vec<Field> = Vec::new();
    let Some(fields) = r.child("fields") else {
        return out;
    };
    for f in fields.all("field") {
        let raw = f.text_of("name").unwrap_or("?").to_string();
        let (lo, width) =
            if let (Some(o), Some(w)) = (f.text_of("bitOffset"), f.text_of("bitWidth")) {
                (parse_num(o), parse_num(w))
            } else if let (Some(l), Some(m)) = (f.text_of("lsb"), f.text_of("msb")) {
                let (l, m) = (parse_num(l), parse_num(m));
                (l, l.zip(m).map(|(l, m)| m.saturating_sub(l) + 1))
            } else if let Some(range) = f.text_of("bitRange") {
                let inner = range.trim_start_matches('[').trim_end_matches(']');
                let (m, l) = inner.split_once(':').unwrap_or((inner, inner));
                let (l, m) = (parse_num(l), parse_num(m));
                (l, l.zip(m).map(|(l, m)| m.saturating_sub(l) + 1))
            } else {
                (None, None)
            };
        let (Some(lo), Some(width)) = (lo, width) else {
            notes.push(format!("{at}.{raw}: no bit position; left out"));
            continue;
        };
        let kind = match (f.text_of("modifiedWriteValues"), f.text_of("readAction")) {
            (_, Some("clear")) => Kind::Rc,
            (Some("oneToClear"), _) => Kind::W1c,
            (Some("zeroToClear"), _) => Kind::RcW0,
            (Some(other), _) if other != "modify" => {
                notes.push(format!(
                    "{at}.{raw}: writes are '{other}', which PHASE has no word for; imported as a plain field"
                ));
                Kind::Plain
            }
            _ => Kind::Plain,
        };
        // A field array (`dim` with `%s` in the name) is written out.
        let copies: Vec<(String, u64)> = match dim_of(f) {
            Some(d) => d
                .index
                .iter()
                .enumerate()
                .map(|(i, x)| (raw.replace("%s", x), lo + i as u64 * d.step))
                .collect(),
            None => vec![(raw.clone(), lo)],
        };
        for (name, lo) in copies {
            let name = ident(&name);
            let hi = lo + width.max(1) - 1;
            if hi >= u64::from(bits) {
                notes.push(format!(
                    "{at}.{name}: bits {lo}..={hi} don't fit the {bits}-bit register; left out"
                ));
                continue;
            }
            let (lo, hi) = (lo as u32, hi as u32);
            if let Some(g) = out.iter().find(|g| g.name == name) {
                notes.push(format!(
                    "{at}.{name}: declared twice; kept the first (bits {}..={})",
                    g.lo, g.hi
                ));
                continue;
            }
            if let Some(g) = out.iter().find(|g| lo <= g.hi && g.lo <= hi) {
                notes.push(format!(
                    "{at}.{name}: shares bits with '{}'; left out",
                    g.name
                ));
                continue;
            }
            out.push(Field { name, lo, hi, kind });
        }
    }
    out.sort_by_key(|f| f.lo);
    // One field that is the whole register says nothing a register
    // doesn't -- and an `address` register must have none.
    if out.len() == 1 && out[0].lo == 0 && out[0].hi + 1 == bits {
        out.clear();
    }
    out
}

/// The registers (and clusters) inside `e`, at `base_offset`, their names
/// prefixed by `prefix`.
fn read_regs(
    e: &Element,
    defaults: Defaults,
    base_offset: u64,
    prefix: &str,
    at: &str,
    notes: &mut Vec<String>,
) -> Vec<Reg> {
    let mut out = Vec::new();
    for c in &e.children {
        match c.name.as_str() {
            "register" => {
                let d = defaults.within(c, notes, at);
                let raw = c.text_of("name").unwrap_or("?");
                let offset =
                    base_offset + c.text_of("addressOffset").and_then(parse_num).unwrap_or(0);
                let description = one_line(c.text_of("description").unwrap_or(""));
                let mk = |name: String, offset: u64, array, notes: &mut Vec<String>| {
                    let full = format!("{at}.{name}");
                    Reg {
                        fields: read_fields(c, d.bits, &full, notes),
                        name,
                        description: description.clone(),
                        offset,
                        bits: d.bits,
                        access: d.access,
                        reset: d.reset,
                        array,
                        folded_from: None,
                    }
                };
                match dim_of(c) {
                    // `NAME[%s]` or `NAME%s` over 0..n: a PHASE array.
                    Some(dim) if is_zero_based(&dim.index) => {
                        let name = ident(&format!(
                            "{prefix}{}",
                            raw.replace("[%s]", "").replace("%s", "x")
                        ));
                        out.push(mk(name, offset, Some((dim.count, dim.step)), notes));
                    }
                    // Named copies (`A`, `B`, ..): written out.
                    Some(dim) => {
                        for (i, x) in dim.index.iter().enumerate() {
                            let name = ident(&format!("{prefix}{}", raw.replace("%s", x)));
                            out.push(mk(name, offset + i as u64 * dim.step, None, notes));
                        }
                    }
                    None => out.push(mk(ident(&format!("{prefix}{raw}")), offset, None, notes)),
                }
            }
            "cluster" => {
                let d = defaults.within(c, notes, at);
                let raw = c
                    .text_of("name")
                    .unwrap_or("?")
                    .replace("[%s]", "")
                    .replace("%s", "x");
                let offset =
                    base_offset + c.text_of("addressOffset").and_then(parse_num).unwrap_or(0);
                let inner = read_regs(c, d, 0, &format!("{prefix}{raw}_"), at, notes);
                match dim_of(c) {
                    Some(dim) if is_zero_based(&dim.index) => {
                        for r in inner {
                            if r.array.is_some() {
                                notes.push(format!(
                                    "{at}.{}: an array inside an array of clusters; left out",
                                    r.name
                                ));
                                continue;
                            }
                            out.push(Reg {
                                offset: offset + r.offset,
                                array: Some((dim.count, dim.step)),
                                ..r
                            });
                        }
                    }
                    Some(dim) => {
                        for (i, x) in dim.index.iter().enumerate() {
                            for r in &inner {
                                out.push(Reg {
                                    name: r.name.replacen(
                                        &format!("{raw}_"),
                                        &format!("{raw}{x}_"),
                                        1,
                                    ),
                                    offset: offset + i as u64 * dim.step + r.offset,
                                    ..r.clone()
                                });
                            }
                        }
                    }
                    None => out.extend(inner.into_iter().map(|r| Reg {
                        offset: offset + r.offset,
                        ..r
                    })),
                }
            }
            _ => {}
        }
    }
    out
}

/// `S0CR`..`S7CR` -> `SxCR[8]`: registers written out one by one that a
/// PHASE array describes exactly (indices 0..n-1, one step apart, the same
/// size, access, reset value and fields).
fn fold_arrays(periph: &str, regs: Vec<Reg>, notes: &mut Vec<String>) -> Vec<Reg> {
    fn split(name: &str) -> Option<(String, u32, String)> {
        let start = name.find(|c: char| c.is_ascii_digit())?;
        let len = name[start..]
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(name.len() - start);
        let idx = name[start..start + len].parse().ok()?;
        Some((
            name[..start].to_string(),
            idx,
            name[start + len..].to_string(),
        ))
    }
    let names: HashSet<String> = regs.iter().map(|r| r.name.clone()).collect();
    let mut groups: BTreeMap<(String, String), Vec<(u32, usize)>> = BTreeMap::new();
    for (i, r) in regs.iter().enumerate() {
        if r.array.is_some() {
            continue;
        }
        if let Some((p, n, s)) = split(&r.name) {
            groups.entry((p, s)).or_default().push((n, i));
        }
    }
    let mut folded: HashMap<usize, Reg> = HashMap::new(); // first member -> the array
    let mut gone: HashSet<usize> = HashSet::new();
    for ((p, s), mut members) in groups {
        members.sort();
        let n = members.len();
        if n < 2
            || members
                .iter()
                .enumerate()
                .any(|(i, (idx, _))| *idx as usize != i)
        {
            continue;
        }
        let first = &regs[members[0].1];
        let step = regs[members[1].1].offset.wrapping_sub(first.offset);
        let alike = members.iter().enumerate().all(|(i, (_, at))| {
            let r = &regs[*at];
            r.offset == first.offset + i as u64 * step
                && r.bits == first.bits
                && r.access == first.access
                && r.reset == first.reset
                && r.fields == first.fields
        });
        let name = format!("{p}x{s}");
        // Placed like an array but described differently: say which.
        let placed = members.iter().enumerate().all(|(i, (_, at))| {
            let r = &regs[*at];
            r.offset == first.offset + i as u64 * step && r.bits == first.bits
        });
        if placed && !alike && step >= u64::from(first.bits / 8) {
            if let Some((_, at)) = members.iter().find(|(_, at)| {
                let r = &regs[*at];
                r.access != first.access || r.reset != first.reset || r.fields != first.fields
            }) {
                notes.push(format!(
                    "{periph}: {}..{} not made an array {name}[{n}]: {} is described differently from {}",
                    first.name,
                    regs[members[n - 1].1].name,
                    regs[*at].name,
                    first.name
                ));
            }
        }
        if !alike
            || step < u64::from(first.bits / 8)
            || p.is_empty() && s.is_empty()
            || names.contains(&name)
        {
            continue;
        }
        let last = &regs[members[n - 1].1].name;
        folded.insert(
            members[0].1,
            Reg {
                name,
                array: Some((n as u32, step)),
                folded_from: Some(format!("{}..{last}", first.name)),
                ..first.clone()
            },
        );
        gone.extend(members.iter().skip(1).map(|(_, at)| *at));
    }
    regs.into_iter()
        .enumerate()
        .filter(|(i, _)| !gone.contains(i))
        .map(|(i, r)| folded.remove(&i).unwrap_or(r))
        .collect()
}

/// Leave out registers PHASE can't place: a size other than 8, 16 or 32
/// bits, or a second register at a place another already has.
fn placeable(name: &str, regs: Vec<Reg>, notes: &mut Vec<String>) -> Vec<Reg> {
    let mut out: Vec<Reg> = Vec::new();
    let mut taken: Vec<(u64, u64, String)> = Vec::new();
    for r in regs {
        if !matches!(r.bits, 8 | 16 | 32) {
            notes.push(format!(
                "{name}.{}: a {}-bit register; left out",
                r.name, r.bits
            ));
            continue;
        }
        if out.iter().any(|o| o.name == r.name) {
            notes.push(format!("{name}.{}: declared twice; kept the first", r.name));
            continue;
        }
        let bytes = u64::from(r.bits / 8);
        let spots: Vec<(u64, u64)> = match r.array {
            None => vec![(r.offset, r.offset + bytes)],
            Some((n, step)) => (0..u64::from(n))
                .map(|i| (r.offset + i * step, r.offset + i * step + bytes))
                .collect(),
        };
        if let Some((_, _, other)) = taken
            .iter()
            .find(|(s, e, _)| spots.iter().any(|(a, b)| a < e && s < b))
        {
            notes.push(format!(
                "{name}.{}: at the same offset as '{other}' (an alternate view of it); left out",
                r.name
            ));
            continue;
        }
        taken.extend(spots.into_iter().map(|(a, b)| (a, b, r.name.clone())));
        out.push(r);
    }
    out.sort_by_key(|r| r.offset);
    out
}

fn format_reg(r: &Reg) -> String {
    let mut s = String::new();
    let mut comment = r.description.clone();
    if let Some(from) = &r.folded_from {
        comment = format!("{comment} ({from})").trim().to_string();
    }
    if !comment.is_empty() {
        s.push_str(&format!("    // {comment}\n"));
    }
    let access = match r.access {
        Access::Read => "read",
        Access::Write => "write",
        Access::ReadWrite => "read write",
    };
    let array = match r.array {
        Some((n, _)) => format!("[{n}]"),
        None => String::new(),
    };
    let step = match r.array {
        Some((_, step)) => format!(" step {step:#x}"),
        None => String::new(),
    };
    let mask = if r.bits >= 64 {
        u64::MAX
    } else {
        (1u64 << r.bits) - 1
    };
    let reset = match r.reset {
        Some(v) => format!(" = {:#x}", v & mask),
        None => String::new(),
    };
    let head = format!(
        "    {}{array}: {access} u{} @ {:#x}{step}{reset}",
        r.name, r.bits, r.offset
    );
    if r.fields.is_empty() {
        s.push_str(&head);
        s.push_str(";\n");
        return s;
    }
    let fields: Vec<String> = r
        .fields
        .iter()
        .map(|f| {
            let pos = if f.lo == f.hi {
                format!("bit {}", f.lo)
            } else {
                format!("bits {}..={}", f.lo, f.hi)
            };
            let kind = match f.kind {
                Kind::Plain => "",
                Kind::W1c => " w1c",
                Kind::Rc => " rc",
                Kind::RcW0 => " rc_w0",
            };
            format!("{}: {pos}{kind}", f.name)
        })
        .collect();
    let one = format!("{head} {{ {} }}", fields.join(", "));
    if one.len() <= 100 {
        s.push_str(&one);
        s.push('\n');
        return s;
    }
    s.push_str(&head);
    s.push_str(" {\n");
    let mut line = String::from("       ");
    for (i, f) in fields.iter().enumerate() {
        let piece = if i + 1 < fields.len() {
            format!(" {f},")
        } else {
            format!(" {f}")
        };
        if line.len() + piece.len() > 96 {
            s.push_str(line.trim_end());
            s.push('\n');
            line = String::from("       ");
        }
        line.push_str(&piece);
    }
    s.push_str(line.trim_end());
    s.push_str("\n    }\n");
    s
}

/// Import `svd` (the text of a CMSIS-SVD file) as a PHASE board file.
pub fn import(svd: &str, opts: &Options) -> Result<Imported, String> {
    let root = xml::parse(svd)?;
    if root.name != "device" {
        return Err(format!(
            "not an SVD file: the root element is <{}>, not <device>",
            root.name
        ));
    }
    let mut notes = Vec::new();
    let chip = root.text_of("name").unwrap_or("?").to_string();
    let defaults = Defaults {
        bits: 32,
        access: Access::ReadWrite,
        reset: None,
    }
    .within(&root, &mut notes, &chip);
    let peripherals = root
        .child("peripherals")
        .ok_or("the SVD file has no <peripherals>")?;

    // Every peripheral, with its own registers (derived ones filled in
    // below), and every interrupt.
    let mut all: Vec<Periph> = Vec::new();
    let mut irqs: BTreeMap<String, u64> = BTreeMap::new();
    let mut raw_regs: HashMap<String, Vec<Reg>> = HashMap::new();
    for p in peripherals.all("peripheral") {
        let name = p.text_of("name").unwrap_or("?").to_string();
        let derived_from = p.attr("derivedFrom").map(str::to_string);
        let d = defaults.within(p, &mut notes, &name);
        let regs = p
            .child("registers")
            .map(|r| read_regs(r, d, 0, "", &name, &mut notes))
            .unwrap_or_default();
        for i in p.all("interrupt") {
            if let (Some(n), Some(v)) = (i.text_of("name"), i.text_of("value").and_then(parse_num))
            {
                irqs.entry(n.to_string()).or_insert(v);
            }
        }
        raw_regs.insert(name.clone(), regs.clone());
        all.push(Periph {
            name,
            derived_from,
            description: one_line(p.text_of("description").unwrap_or("")),
            group: p.text_of("groupName").unwrap_or("").to_string(),
            base: p.text_of("baseAddress").and_then(parse_num).unwrap_or(0),
            regs,
        });
    }
    // A derived peripheral without registers of its own has its base's;
    // its layout's source is the base.
    let mut source: HashMap<String, String> = HashMap::new();
    for p in &mut all {
        let src = match (&p.derived_from, p.regs.is_empty()) {
            (Some(b), true) => {
                p.regs = raw_regs.get(b).cloned().unwrap_or_default();
                b.clone()
            }
            _ => p.name.clone(),
        };
        source.insert(p.name.clone(), src);
    }
    let base_info: HashMap<String, (String, String)> = all
        .iter()
        .map(|p| (p.name.clone(), (p.description.clone(), p.group.clone())))
        .collect();

    let chosen: Vec<&Periph> = match &opts.only {
        None => all.iter().collect(),
        Some(names) => {
            let mut out = Vec::new();
            for n in names {
                match all.iter().find(|p| p.name.eq_ignore_ascii_case(n)) {
                    Some(p) => out.push(p),
                    None => {
                        let mut known: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
                        known.sort();
                        return Err(format!(
                            "{chip} has no peripheral '{n}' (it has {})",
                            known.join(", ")
                        ));
                    }
                }
            }
            out
        }
    };

    // Two or more chosen peripherals with one layout: a device type.
    let mut by_source: BTreeMap<String, Vec<&Periph>> = BTreeMap::new();
    for p in &chosen {
        by_source
            .entry(source[&p.name].clone())
            .or_default()
            .push(p);
    }
    let all_names: HashSet<String> = all.iter().map(|p| ident(&p.name)).collect();
    let mut groups_with_types: HashMap<String, usize> = HashMap::new();
    for (src, ps) in &by_source {
        if ps.len() >= 2 {
            *groups_with_types
                .entry(base_info[src].1.clone())
                .or_default() += 1;
        }
    }
    let type_name = |src: &str| -> String {
        let group = &base_info[src].1;
        let candidate = ident(group);
        if !group.is_empty()
            && groups_with_types.get(group) == Some(&1)
            && !all_names.contains(&candidate)
        {
            candidate
        } else {
            ident(&format!("{src}_Type"))
        }
    };

    let mut out = String::new();
    out.push_str(&format!(
        "// {chip}: imported from its CMSIS-SVD file by `phase import-svd`.\n//\n\
         // SVD doesn't say which registers hold DMA addresses: mark those\n\
         // `address` (and `until REG.FIELD == N`, when the device is done) by hand.\n\
         // Nor, unless the vendor filled in modifiedWriteValues, which flags are\n\
         // cleared by writing 1 (`w1c`) or 0 (`rc_w0`): check the reference\n\
         // manual and mark them, so PHASE can catch field writes that clear them.\n\n"
    ));
    let mut devices = 0;
    let mut registers = 0;
    let mut emitted_types: HashSet<String> = HashSet::new();
    let mut seen_sources: HashSet<String> = HashSet::new();
    for p in &chosen {
        let src = &source[&p.name];
        let shared = by_source[src].len() >= 2;
        let regs = fold_arrays(&p.name, p.regs.clone(), &mut notes);
        let regs = placeable(&p.name, regs, &mut notes);
        if regs.is_empty() {
            notes.push(format!(
                "{}: no registers PHASE can declare; left out",
                p.name
            ));
            continue;
        }
        if shared {
            let ty = type_name(src);
            if emitted_types.insert(ty.clone()) {
                let desc = &base_info[src].0;
                out.push_str(&format!(
                    "// {}{}\n",
                    if desc.is_empty() {
                        ty.clone()
                    } else {
                        desc.clone()
                    },
                    if src != &p.name || by_source[src].len() > 1 {
                        format!(
                            " (the layout of {})",
                            by_source[src]
                                .iter()
                                .map(|q| q.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else {
                        String::new()
                    }
                ));
                out.push_str(&format!("device type {ty} {{\n"));
                for r in &regs {
                    out.push_str(&format_reg(r));
                }
                out.push_str("}\n\n");
                registers += regs.len();
            }
            if seen_sources.insert(src.clone()) {
                for q in &by_source[src] {
                    out.push_str(&format!(
                        "device {}: {ty} @ {:#010x};\n",
                        ident(&q.name),
                        q.base
                    ));
                    devices += 1;
                }
                out.push('\n');
            }
            continue;
        }
        let desc = if p.description.is_empty() {
            base_info[src].0.clone()
        } else {
            p.description.clone()
        };
        if !desc.is_empty() {
            out.push_str(&format!("// {desc}\n"));
        }
        out.push_str(&format!(
            "device {} @ {:#010x} {{\n",
            ident(&p.name),
            p.base
        ));
        for r in &regs {
            out.push_str(&format_reg(r));
        }
        out.push_str("}\n\n");
        devices += 1;
        registers += regs.len();
    }

    if !irqs.is_empty() {
        out.push_str("// Interrupt numbers, named as CMSIS names them: `interrupt fn h() @ irq\n// NAME_IRQn` is also emitted as NAME_IRQHandler.\n");
        let mut by_value: Vec<(&String, &u64)> = irqs.iter().collect();
        by_value.sort_by_key(|(n, v)| (**v, (*n).clone()));
        for (n, v) in by_value {
            out.push_str(&format!("const {}_IRQn: u32 = {v};\n", ident(n)));
        }
    }
    Ok(Imported {
        text: out,
        notes,
        devices,
        registers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<device schemaVersion="1.1">
  <name>DEMO</name>
  <size>0x20</size>
  <resetValue>0x0</resetValue>
  <peripherals>
    <peripheral>
      <name>DMA2</name>
      <description>DMA controller</description>
      <groupName>DMA</groupName>
      <baseAddress>0x40026400</baseAddress>
      <interrupt><name>DMA2_Stream0</name><value>56</value></interrupt>
      <registers>
        <register>
          <name>LISR</name><description>low interrupt status register</description>
          <addressOffset>0x0</addressOffset><access>read-only</access>
          <fields>
            <field><name>TCIF0</name><bitOffset>5</bitOffset><bitWidth>1</bitWidth></field>
          </fields>
        </register>
        <register>
          <name>LIFCR</name><addressOffset>0x8</addressOffset><access>write-only</access>
        </register>
        <register>
          <name>S0CR</name><addressOffset>0x10</addressOffset>
          <fields>
            <field><name>EN</name><bitOffset>0</bitOffset><bitWidth>1</bitWidth></field>
            <field><name>CHSEL</name><bitRange>[27:25]</bitRange></field>
          </fields>
        </register>
        <register>
          <name>S0PAR</name><addressOffset>0x18</addressOffset>
          <fields><field><name>PA</name><bitOffset>0</bitOffset><bitWidth>32</bitWidth></field></fields>
        </register>
        <register>
          <name>S1CR</name><addressOffset>0x28</addressOffset>
          <fields>
            <field><name>EN</name><bitOffset>0</bitOffset><bitWidth>1</bitWidth></field>
            <field><name>CHSEL</name><lsb>25</lsb><msb>27</msb></field>
          </fields>
        </register>
        <register>
          <name>S1PAR</name><addressOffset>0x30</addressOffset>
          <fields><field><name>PA</name><bitOffset>0</bitOffset><bitWidth>32</bitWidth></field></fields>
        </register>
      </registers>
    </peripheral>
    <peripheral derivedFrom="DMA2">
      <name>DMA1</name>
      <baseAddress>0x40026000</baseAddress>
      <interrupt><name>DMA1_Stream0</name><value>11</value></interrupt>
    </peripheral>
    <peripheral>
      <name>TIM2</name>
      <description>General purpose timer</description>
      <baseAddress>0x40000000</baseAddress>
      <interrupt><name>TIM2</name><value>28</value></interrupt>
      <registers>
        <register>
          <name>SR</name><addressOffset>0x10</addressOffset>
          <fields>
            <field><name>UIF</name><bitOffset>0</bitOffset><bitWidth>1</bitWidth><modifiedWriteValues>zeroToClear</modifiedWriteValues></field>
            <field><name>CC1IF</name><bitOffset>1</bitOffset><bitWidth>1</bitWidth><modifiedWriteValues>oneToClear</modifiedWriteValues></field>
            <field><name>OVR</name><bitOffset>2</bitOffset><bitWidth>1</bitWidth><readAction>clear</readAction></field>
            <field><name>BAD</name><bitOffset>2</bitOffset><bitWidth>2</bitWidth></field>
          </fields>
        </register>
        <register>
          <name>CCMR1_Output</name><addressOffset>0x18</addressOffset>
        </register>
        <register>
          <name>CCMR1_Input</name><addressOffset>0x18</addressOffset>
        </register>
        <register>
          <dim>4</dim><dimIncrement>0x4</dimIncrement>
          <name>CCR[%s]</name><addressOffset>0x34</addressOffset>
        </register>
        <cluster>
          <dim>2</dim><dimIncrement>0x10</dimIncrement>
          <name>CH[%s]</name><addressOffset>0x80</addressOffset>
          <register><name>CTL</name><addressOffset>0x0</addressOffset></register>
          <register><name>VAL</name><addressOffset>0x4</addressOffset><size>16</size></register>
        </cluster>
      </registers>
    </peripheral>
  </peripherals>
</device>
"#;

    fn imported(only: Option<&[&str]>) -> Imported {
        import(
            SVD,
            &Options {
                only: only.map(|o| o.iter().map(|s| s.to_string()).collect()),
            },
        )
        .unwrap()
    }

    #[test]
    fn a_board_file_from_an_svd_file() {
        let i = imported(None);
        for needle in [
            // Two peripherals with one layout: a type and two instances.
            "device type DMA {",
            "    LISR: read u32 @ 0x0 = 0x0 { TCIF0: bit 5 }",
            "    LIFCR: write u32 @ 0x8 = 0x0;",
            // Written out one by one, folded into arrays.
            "    // (S0CR..S1CR)\n    SxCR[2]: read write u32 @ 0x10 step 0x18 = 0x0 { EN: bit 0, CHSEL: bits 25..=27 }",
            // One whole-register field is no field: `address`-ready.
            "    SxPAR[2]: read write u32 @ 0x18 step 0x18 = 0x0;",
            "device DMA2: DMA @ 0x40026400;\ndevice DMA1: DMA @ 0x40026000;",
            // Write semantics, where the vendor gives them.
            "    SR: read write u32 @ 0x10 = 0x0 { UIF: bit 0 rc_w0, CC1IF: bit 1 w1c, OVR: bit 2 rc }",
            // SVD arrays and clusters.
            "    CCR[4]: read write u32 @ 0x34 step 0x4 = 0x0;",
            "    CH_CTL[2]: read write u32 @ 0x80 step 0x10 = 0x0;",
            "    CH_VAL[2]: read write u16 @ 0x84 step 0x10 = 0x0;",
            // Interrupts by number, CMSIS names.
            "const DMA1_Stream0_IRQn: u32 = 11;\nconst TIM2_IRQn: u32 = 28;\nconst DMA2_Stream0_IRQn: u32 = 56;",
        ] {
            assert!(i.text.contains(needle), "missing {needle:?} in\n{}", i.text);
        }
        assert!(
            i.notes
                .iter()
                .any(|n| n.contains("TIM2.SR.BAD: shares bits with 'OVR'")),
            "{:?}",
            i.notes
        );
        assert!(
            i.notes
                .iter()
                .any(|n| n.contains("TIM2.CCMR1_Input: at the same offset as 'CCMR1_Output'")),
            "{:?}",
            i.notes
        );
        assert_eq!((i.devices, i.registers), (3, 4 + 5));
    }

    #[test]
    fn the_board_file_is_valid_phase() {
        let i = imported(None);
        let src = format!("{}\nfn main() {{}}\n", i.text);
        let program = phase_parser::parse(&src).unwrap_or_else(|e| panic!("{e:?}\n{src}"));
        phase_analysis::analyze(&program).unwrap_or_else(|e| panic!("{e:?}\n{src}"));
    }

    #[test]
    fn only_the_chosen_peripherals() {
        // One of two sharing a layout: a plain device, no type.
        let i = imported(Some(&["dma1"]));
        assert!(i.text.contains("device DMA1 @ 0x40026000 {"), "{}", i.text);
        assert!(!i.text.contains("device type"));
        assert!(!i.text.contains("TIM2 @"));
        let err = import(
            SVD,
            &Options {
                only: Some(vec!["UART9".into()]),
            },
        )
        .unwrap_err();
        assert!(
            err.contains("DEMO has no peripheral 'UART9' (it has DMA1, DMA2, TIM2)"),
            "{err}"
        );
    }
}
