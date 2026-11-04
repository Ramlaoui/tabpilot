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

const TOKEN_SECRET = "tabpilot.commonthread.token";
const ENABLED_KEY = "tabpilot.enabled";

// Settings forwarded to tabpilot-lsp as environment variables; empty values are not set.
const SETTING_ENV: Record<string, string> = {
  provider: "TABPILOT_PROVIDER",
  harness: "TABPILOT_HARNESS",
  "claude.model": "TABPILOT_CLAUDE_MODEL",
  "claude.effort": "TABPILOT_CLAUDE_EFFORT",
  "commonthread.url": "TABPILOT_COMMONTHREAD_URL",
  "commonthread.profile": "TABPILOT_COMMONTHREAD_PROFILE",
  "commonthread.model": "TABPILOT_COMMONTHREAD_MODEL",
  "commonthread.effort": "TABPILOT_COMMONTHREAD_EFFORT",
};

let client: LanguageClient | undefined;
let startFailed = false;
let restarting: Promise<void> = Promise.resolve();
let statusBarItem: vscode.StatusBarItem;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const outputChannel = vscode.window.createOutputChannel("Tabpilot", { log: true });
  context.subscriptions.push(outputChannel);

  statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right);
  statusBarItem.command = "tabpilot.restartLanguageServer";
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  const restartCommand = vscode.commands.registerCommand(
    "tabpilot.restartLanguageServer",
    () => restartClient(context, outputChannel),
  );
  context.subscriptions.push(restartCommand);

  const setServerPathCommand = vscode.commands.registerCommand(
    "tabpilot.setServerPath",
    () => setServerPath(context, outputChannel),
  );
  context.subscriptions.push(setServerPathCommand);

  context.subscriptions.push(
    vscode.commands.registerCommand("tabpilot.setCommonthreadToken", () =>
      setCommonthreadToken(context),
    ),
    vscode.commands.registerCommand("tabpilot.showStatus", () => showStatus()),
    vscode.commands.registerCommand("tabpilot.toggle", async () => {
      await context.globalState.update(ENABLED_KEY, !isEnabled(context));
      updateStatus(context);
    }),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration("tabpilot")) {
        void restartClient(context, outputChannel);
      }
    }),
    context.secrets.onDidChange((event) => {
      if (event.key === TOKEN_SECRET) {
        void restartClient(context, outputChannel);
      }
    }),
  );

  updateStatus(context);
  void restartClient(context, outputChannel);
}

export async function deactivate(): Promise<void> {
  await stopClient();
}

async function startClient(
  context: vscode.ExtensionContext,
  outputChannel: vscode.LogOutputChannel,
): Promise<LanguageClient | undefined> {
  const serverPath = resolveServerPath(context);
  if (!fs.existsSync(serverPath)) {
    outputChannel.appendLine(`tabpilot-lsp not found at ${serverPath}`);
    outputChannel.appendLine("Run `cargo build --bin tabpilot-lsp` or set tabpilot.serverPath.");
    startFailed = true;
    return undefined;
  }

  const serverOptions: ServerOptions = {
    command: serverPath,
    transport: TransportKind.stdio,
    options: { env: await serverEnv(context) },
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file" }],
    outputChannel,
    middleware: {
      provideInlineCompletionItems: (document, position, inlineContext, token, next) =>
        isEnabled(context) ? next(document, position, inlineContext, token) : undefined,
    },
  };

  const nextClient = new LanguageClient(
    "tabpilot",
    "Tabpilot",
    serverOptions,
    clientOptions,
  );
  context.subscriptions.push(nextClient);
  nextClient.onDidChangeState(() => updateStatus(context));

  nextClient.start().catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(`failed to start tabpilot-lsp: ${message}`);
    startFailed = true;
    updateStatus(context);
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

function restartClient(
  context: vscode.ExtensionContext,
  outputChannel: vscode.LogOutputChannel,
): Promise<void> {
  // Serialize restarts so rapid setting or token changes never run two servers at once.
  restarting = restarting.then(async () => {
    try {
      await stopClient();
    } catch (error: unknown) {
      outputChannel.appendLine(`failed to stop tabpilot-lsp: ${String(error)}`);
    }
    startFailed = false;
    client = await startClient(context, outputChannel);
    updateStatus(context);
  });
  return restarting;
}

async function serverEnv(context: vscode.ExtensionContext): Promise<NodeJS.ProcessEnv> {
  const configuration = vscode.workspace.getConfiguration("tabpilot");
  const env: NodeJS.ProcessEnv = { ...process.env };
  for (const [setting, name] of Object.entries(SETTING_ENV)) {
    const value = configuration.get<string>(setting)?.trim();
    if (value) {
      env[name] = value;
    }
  }

  // Only touch the OS keyring when the token is actually needed.
  if (env.TABPILOT_PROVIDER === "commonthread") {
    const token = await context.secrets.get(TOKEN_SECRET);
    if (token) {
      env.TABPILOT_COMMONTHREAD_TOKEN = token;
    }
  }
  return env;
}

async function showStatus(): Promise<void> {
  if (!client?.isRunning()) {
    vscode.window.showWarningMessage("Tabpilot is not running.");
    return;
  }
  const status = await client.sendRequest<{
    provider: string;
    details: Record<string, string | null> | null;
    last_error: string | null;
  }>("workspace/executeCommand", { command: "tabpilot.status", arguments: [] });
  const fields = { ...status.details, "last error": status.last_error };
  const lines = [`provider: ${status.provider}`].concat(
    Object.entries(fields)
      .filter(([, value]) => value)
      .map(([key, value]) => `${key}: ${value}`),
  );
  vscode.window.showInformationMessage("Tabpilot status", { modal: true, detail: lines.join("\n") });
}

async function setCommonthreadToken(context: vscode.ExtensionContext): Promise<void> {
  const token = await vscode.window.showInputBox({
    title: "Tabpilot: Set Commonthread Token",
    prompt: "Commonthread API token. Leave empty to remove the stored token.",
    password: true,
    ignoreFocusOut: true,
  });
  if (token === undefined) {
    return;
  }

  if (token.trim().length > 0) {
    await context.secrets.store(TOKEN_SECRET, token.trim());
  } else {
    await context.secrets.delete(TOKEN_SECRET);
  }
}

function isEnabled(context: vscode.ExtensionContext): boolean {
  return context.globalState.get<boolean>(ENABLED_KEY, true);
}

function updateStatus(context: vscode.ExtensionContext): void {
  const provider = vscode.workspace.getConfiguration("tabpilot").get<string>("provider");
  const enabled = isEnabled(context);
  const state = startFailed
    ? "error"
    : client?.state === State.Running
      ? "running"
      : client?.state === State.Starting
        ? "starting"
        : "stopped";
  const icons = {
    running: "$(sparkle)",
    starting: "$(sync~spin)",
    stopped: "$(debug-stop)",
    error: "$(error)",
  };

  statusBarItem.text = `${enabled ? icons[state] : "$(circle-slash)"} Tabpilot: ${provider}`;
  statusBarItem.tooltip =
    `Tabpilot (${provider}) is ${state}` +
    (enabled ? "" : "; inline completions are disabled") +
    ". Click to restart.";
  statusBarItem.backgroundColor =
    state === "error" ? new vscode.ThemeColor("statusBarItem.errorBackground") : undefined;
}

async function setServerPath(
  context: vscode.ExtensionContext,
  outputChannel: vscode.LogOutputChannel,
): Promise<void> {
  const configuration = vscode.workspace.getConfiguration("tabpilot");
  const configuredPath = configuration.get<string>("serverPath") ?? "";
  const fallbackPath = resolveDefaultServerPath(context);
  const currentPath = configuredPath.trim().length > 0 ? configuredPath : fallbackPath;

  const selectedPath = await vscode.window.showInputBox({
    title: "Tabpilot: Set Server Path",
    prompt: "Absolute path to tabpilot-lsp. Leave empty to restore the default path.",
    value: currentPath,
    valueSelection: [0, currentPath.length],
    validateInput: (value) => {
      const trimmed = value.trim();
      if (trimmed.length === 0) {
        return undefined;
      }

      return path.isAbsolute(trimmed) ? undefined : "Enter an absolute path.";
    },
  });

  if (selectedPath === undefined) {
    return;
  }

  const nextPath = selectedPath.trim();
  const target =
    vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders.length > 0
      ? vscode.ConfigurationTarget.Workspace
      : vscode.ConfigurationTarget.Global;

  await configuration.update(
    "serverPath",
    nextPath.length > 0 ? nextPath : undefined,
    target,
  );

  const effectivePath = nextPath.length > 0 ? nextPath : fallbackPath;
  if (!fs.existsSync(effectivePath)) {
    vscode.window.showWarningMessage(`tabpilot-lsp not found at ${effectivePath}`);
  }

  outputChannel.appendLine(`tabpilot.serverPath set to ${effectivePath}`);
  await restartClient(context, outputChannel);
}

function resolveServerPath(context: vscode.ExtensionContext): string {
  const configuredPath = vscode.workspace
    .getConfiguration("tabpilot")
    .get<string>("serverPath");

  if (configuredPath && configuredPath.trim().length > 0) {
    return configuredPath;
  }

  return resolveDefaultServerPath(context);
}

function resolveDefaultServerPath(context: vscode.ExtensionContext): string {
  return path.resolve(context.extensionPath, "..", "target", "debug", "tabpilot-lsp");
}
