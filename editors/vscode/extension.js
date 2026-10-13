// PHASE for VS Code: starts `phase lsp` (the compiler's own language
// server) for .phase files, so the diagnostics `phase check` prints show up
// inline as you type. Highlighting comes from syntaxes/ and works without it.
"use strict";

const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

function activate(context) {
  const command = vscode.workspace.getConfiguration("phase").get("serverPath", "phase");
  if (!command) {
    return;
  }
  const server = { command, args: ["lsp"] };
  client = new LanguageClient(
    "phase",
    "PHASE",
    { run: server, debug: server },
    { documentSelector: [{ scheme: "file", language: "phase" }] }
  );
  client.start().catch((err) => {
    vscode.window.showWarningMessage(
      `PHASE: could not start '${command} lsp' (${err.message}). ` +
        "Install the phase compiler or set phase.serverPath; highlighting still works."
    );
  });
  context.subscriptions.push({ dispose: () => client && client.stop() });
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
