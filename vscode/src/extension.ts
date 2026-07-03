import * as fs from "node:fs";
import * as path from "node:path";

import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  State,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const outputChannel = vscode.window.createOutputChannel("Tabpilot", { log: true });
  context.subscriptions.push(outputChannel);

  const restartCommand = vscode.commands.registerCommand(
    "tabpilot.restartLanguageServer",
    async () => {
      await stopClient();
      client = startClient(context, outputChannel);
    },
  );
  context.subscriptions.push(restartCommand);

  client = startClient(context, outputChannel);
}

export async function deactivate(): Promise<void> {
  await stopClient();
}

function startClient(
  context: vscode.ExtensionContext,
  outputChannel: vscode.LogOutputChannel,
): LanguageClient | undefined {
  const serverPath = resolveServerPath(context);
  if (!fs.existsSync(serverPath)) {
    outputChannel.appendLine(`tabpilot-lsp not found at ${serverPath}`);
    outputChannel.appendLine("Run `cargo build --bin tabpilot-lsp` or set tabpilot.serverPath.");
    return undefined;
  }

  const serverOptions: ServerOptions = {
    command: serverPath,
    transport: TransportKind.stdio,
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file" }],
    outputChannel,
  };

  const nextClient = new LanguageClient(
    "tabpilot",
    "Tabpilot",
    serverOptions,
    clientOptions,
  );
  context.subscriptions.push(nextClient);

  nextClient.start().catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(`failed to start tabpilot-lsp: ${message}`);
  });

  return nextClient;
}

async function stopClient(): Promise<void> {
  if (!client || client.state === State.Stopped) {
    return;
  }

  const oldClient = client;
  client = undefined;
  await oldClient.stop();
}

function resolveServerPath(context: vscode.ExtensionContext): string {
  const configuredPath = vscode.workspace
    .getConfiguration("tabpilot")
    .get<string>("serverPath");

  if (configuredPath && configuredPath.trim().length > 0) {
    return configuredPath;
  }

  return path.resolve(context.extensionPath, "..", "target", "debug", "tabpilot-lsp");
}
