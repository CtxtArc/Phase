//! Loading a program from files (v2.1): the file being compiled, and every
//! file it imports, transitively.
//!
//! `import "path.phase";` is resolved relative to the importing file's
//! directory. Each file is loaded once, however many files import it (so
//! cycles are harmless), and its items take the place of the `import` --
//! like an include guard, but without a preprocessor: one program, one
//! namespace. Spans carry the number of the file they came from: 0 for
//! the file being compiled, then each imported file in the order it was
//! first reached.

use crate::{parse_file, ParseError};
use phase_ast::{Item, Program, Span};
use std::path::{Component, Path, PathBuf};

/// One source file of a loaded program.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// The path as it should be shown: the root as given, and imports
    /// joined onto their importer's directory.
    pub path: PathBuf,
    pub src: String,
    /// For an imported file, the `import` in the file being compiled that
    /// (directly or through other imports) brought it in.
    pub via: Option<Span>,
}

/// A program and the files it was loaded from (`files[n]` is file `n`).
#[derive(Debug, Clone)]
pub struct Loaded {
    pub program: Program,
    pub files: Vec<SourceFile>,
}

/// A failure to load: a file that can't be read or parsed. `files` holds
/// what was loaded so far, so the error's span can be shown in its file.
#[derive(Debug, Clone)]
pub struct LoadError {
    pub error: ParseError,
    pub files: Vec<SourceFile>,
}

/// Load `root` and everything it imports, reading each file with `read`
/// (the language server reads open, unsaved documents this way).
pub fn load(
    root: &Path,
    read: &mut dyn FnMut(&Path) -> std::io::Result<String>,
) -> Result<Loaded, LoadError> {
    let mut l = Loader {
        read,
        files: Vec::new(),
        keys: Vec::new(),
        items: Vec::new(),
    };
    let src = match (l.read)(root) {
        Ok(s) => s,
        Err(e) => {
            return Err(LoadError {
                error: ParseError {
                    message: format!("could not read '{}': {e}", root.display()),
                    span: Span::default(),
                },
                files: Vec::new(),
            })
        }
    };
    l.keys.push(key(root));
    l.files.push(SourceFile {
        path: root.to_path_buf(),
        src,
        via: None,
    });
    match l.expand(0, None) {
        Ok(()) => Ok(Loaded {
            program: {
                let mut p = Program { items: l.items };
                phase_ast::resolve_irq_names(&mut p);
                p
            },
            files: l.files,
        }),
        Err(error) => Err(LoadError {
            error,
            files: l.files,
        }),
    }
}

struct Loader<'r> {
    read: &'r mut dyn FnMut(&Path) -> std::io::Result<String>,
    files: Vec<SourceFile>,
    /// Identity of each loaded file, for loading it only once.
    keys: Vec<PathBuf>,
    items: Vec<Item>,
}

impl Loader<'_> {
    /// Parse file `id` and append its items, loading each import in place.
    fn expand(&mut self, id: u32, via: Option<Span>) -> Result<(), ParseError> {
        let program = parse_file(&self.files[id as usize].src, id)?;
        let dir = self.files[id as usize]
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        for item in program.items {
            let Item::Import(imp) = item else {
                self.items.push(item);
                continue;
            };
            let path = normalize(&dir.join(&imp.path));
            let k = key(&path);
            if self.keys.contains(&k) {
                continue; // already loaded (or being loaded)
            }
            let src = (self.read)(&path).map_err(|e| ParseError {
                message: format!(
                    "cannot import \"{}\": could not read '{}': {e}",
                    imp.path,
                    path.display()
                ),
                span: imp.span,
            })?;
            let new_id = self.files.len() as u32;
            // Imports in the file being compiled are what an editor can
            // point at for errors in the files they bring in.
            let via = via.or(Some(imp.span));
            self.keys.push(k);
            self.files.push(SourceFile { path, src, via });
            self.expand(new_id, via)?;
        }
        Ok(())
    }
}

/// `a/./b/../c` -> `a/c`, without touching the file system.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            // `..` cancels a preceding name, but not a preceding `..` (or
            // the root): `../../x` stays two levels up.
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A file's identity: its canonical path when it exists on disk, else its
/// absolute normalized path.
fn key(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| {
        let abs = std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf());
        normalize(&abs)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fs(files: &[(&str, &str)]) -> impl FnMut(&Path) -> std::io::Result<String> {
        let map: HashMap<PathBuf, String> = files
            .iter()
            .map(|(p, s)| (PathBuf::from(p), s.to_string()))
            .collect();
        move |p: &Path| {
            map.get(p).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "No such file or directory")
            })
        }
    }

    #[test]
    fn imports_are_loaded_once_relative_to_the_importer() {
        let mut read = fs(&[
            (
                "app/main.phase",
                "import \"lib/a.phase\";\nimport \"lib/b.phase\";\nfn main() {}\n",
            ),
            ("app/lib/a.phase", "import \"b.phase\";\nextern fn fa();\n"),
            (
                "app/lib/b.phase",
                "import \"./a.phase\";\nextern fn fb();\n",
            ),
        ]);
        let l = load(Path::new("app/main.phase"), &mut read).unwrap();
        let names: Vec<String> = l
            .program
            .items
            .iter()
            .map(|i| match i {
                Item::Fn(f) => f.name.clone(),
                Item::ExternFn(f) => f.name.clone(),
                _ => "?".into(),
            })
            .collect();
        // a's import of b is expanded in place, before a's own items.
        assert_eq!(names, ["fb", "fa", "main"]);
        let paths: Vec<_> = l.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("app/main.phase"),
                PathBuf::from("app/lib/a.phase"),
                PathBuf::from("app/lib/b.phase"),
            ]
        );
        // Spans know their file; both imported files are attributed to the
        // first import of main.phase.
        let Item::ExternFn(fb) = &l.program.items[0] else {
            panic!()
        };
        assert_eq!(fb.name_span.file, 2);
        assert_eq!(l.files[2].via.map(|s| s.line), Some(1));
    }

    #[test]
    fn a_missing_import_or_a_parse_error_names_its_file() {
        let mut read = fs(&[("m.phase", "fn main() {}\nimport \"nope.phase\";\n")]);
        let e = load(Path::new("m.phase"), &mut read).unwrap_err();
        assert!(
            e.error.message.starts_with("cannot import \"nope.phase\""),
            "{}",
            e.error.message
        );
        assert_eq!((e.error.span.file, e.error.span.line), (0, 2));

        let mut read = fs(&[
            ("m.phase", "import \"bad.phase\";\n"),
            ("bad.phase", "fn {"),
        ]);
        let e = load(Path::new("m.phase"), &mut read).unwrap_err();
        assert_eq!(e.error.span.file, 1);
        assert_eq!(e.files[1].path, PathBuf::from("bad.phase"));
    }

    #[test]
    fn normalize_keeps_leading_parent_dirs() {
        assert_eq!(
            normalize(Path::new("../../a/b")),
            PathBuf::from("../../a/b")
        );
        assert_eq!(normalize(Path::new("x/../../a")), PathBuf::from("../a"));
        assert_eq!(normalize(Path::new("x/./y/../z")), PathBuf::from("x/z"));
        assert_eq!(normalize(Path::new("/../a")), PathBuf::from("/a"));
    }
}
