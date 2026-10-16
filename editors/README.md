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
  devices (with their registers) as document symbols. Hover over a buffer
  to see who owns it at that line and the statements that got it there;
  over a function, register, field or event to see its declaration.
  Go-to-definition jumps to declarations, in imported files too. It needs
  nothing but the `phase` binary.

## VS Code

> **Untested so far.** The grammar is checked against every example, and
> `phase lsp` by a full session in the test suite, but the extension
> itself hasn't been run in VS Code yet. Neovim (below) is the tested
> setup.

The extension in [`vscode/`](vscode/) highlights `.phase` files and starts
`phase lsp`. With `phase` on your `PATH` (or `phase.serverPath` set to it):

```sh
cd editors/vscode
npm install                      # vscode-languageclient
npx @vscode/vsce package         # -> phase-language-2.3.0.vsix
code --install-extension phase-language-2.3.0.vsix
```

Setting `phase.serverPath` to an empty string keeps highlighting only.

## Neovim (0.11+), including LazyVim

Tested with Neovim 0.11 and lazy.nvim. Save as
`~/.config/nvim/lua/plugins/phase.lua` (LazyVim loads every file there),
with your clone's path on the first line:

```lua
local phase_repo = vim.fn.expand("~/path/to/phase")

-- Language server: needs only the `phase` binary on PATH.
vim.filetype.add({ extension = { phase = "phase" } })
vim.lsp.config("phase", {
  cmd = { "phase", "lsp" },
  filetypes = { "phase" },
  root_markers = { "runtime", ".git" },
})
vim.lsp.enable("phase")

-- Highlighting: the repo's Vim syntax files, skipped if the path is wrong.
local syntax_dir = phase_repo .. "/editors/vim"
return {
  {
    dir = syntax_dir,
    name = "phase-syntax",
    lazy = false,
    cond = vim.fn.isdirectory(syntax_dir) == 1,
  },
}
```

Without lazy.nvim, put the first block in your `init.lua` and add
`editors/vim` to `runtimepath` (`vim.opt.rtp:append(...)`). Errors appear
inline; `vim.diagnostic.open_float()` (LazyVim: `<leader>cd`) shows one
with its notes, and `]d`/`[d` jump between them. An error inside an
imported file is shown on its `import` line. `K` shows the hover (a
buffer's owner and history) and `gd` goes to a definition.

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
