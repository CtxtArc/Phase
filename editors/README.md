# Editor support

Two pieces, usable separately:

- **Syntax highlighting** — a TextMate grammar
  ([`vscode/syntaxes/phase.tmLanguage.json`](vscode/syntaxes/phase.tmLanguage.json),
  also usable by any editor that reads TextMate grammars) and a Vim syntax
  file ([`vim/`](vim/)).
- **`phase lsp`** — the compiler's own language server, on stdin/stdout.
  It runs the same parser and analysis as `phase check` on every change
  and publishes the same diagnostics: the same underlines, warnings as
  warnings, and every note ("'b' is moved to @DMA here") as related
  information you can jump to. It also lists a file's functions and
  devices (with their registers) as document symbols. It needs nothing
  but the `phase` binary.

## VS Code

The extension in [`vscode/`](vscode/) highlights `.phase` files and starts
`phase lsp`. With `phase` on your `PATH` (or `phase.serverPath` set to it):

```sh
cd editors/vscode
npm install                      # vscode-languageclient
npx @vscode/vsce package         # -> phase-language-2.0.0.vsix
code --install-extension phase-language-2.0.0.vsix
```

Setting `phase.serverPath` to an empty string keeps highlighting only.

## Neovim (0.11+)

```lua
vim.filetype.add({ extension = { phase = "phase" } })
vim.lsp.config("phase", { cmd = { "phase", "lsp" }, filetypes = { "phase" } })
vim.lsp.enable("phase")
```

For highlighting, add `editors/vim` to `runtimepath`
(`vim.opt.rtp:append("/path/to/phase/editors/vim")`).

## Vim

Copy `editors/vim/ftdetect` and `editors/vim/syntax` into `~/.vim/`. Any
LSP plugin can run `phase lsp` for `phase` buffers.

## Helix

In `languages.toml`:

```toml
[language-server.phase]
command = "phase"
args = ["lsp"]

[[language]]
name = "phase"
scope = "source.phase"
file-types = ["phase"]
comment-token = "//"
language-servers = ["phase"]
```

## Emacs (eglot)

```elisp
(define-derived-mode phase-mode prog-mode "PHASE")
(add-to-list 'auto-mode-alist '("\\.phase\\'" . phase-mode))
(add-to-list 'eglot-server-programs '(phase-mode "phase" "lsp"))
```

## Testing

`python3 editors/vscode/test/grammar_check.py` tokenizes every example
with the TextMate grammar and checks the scopes it assigns (CI runs it);
`cargo test -p phase_driver --test lsp` drives a full `phase lsp` session
over pipes.
