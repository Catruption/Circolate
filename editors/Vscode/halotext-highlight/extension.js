const vscode = require('vscode');
const net = require('net');
const path = require('path');

let client = null;
let status = null;
let noteDecoration = null;
let buffer = '';
let activeSource = null;
let latestMessage = null;
let decoratedEditor = null;
let lastDecorationKey = '';
let reconnectTimer = null;
let requestedSocketPath = null;
let connectionWarningShown = false;

function normalizedPath(value) {
    return path.resolve(value).normalize();
}

function clearHighlight() {
    if (decoratedEditor && noteDecoration) decoratedEditor.setDecorations(noteDecoration, []);
    decoratedEditor = null;
    lastDecorationKey = '';
}

function render(msg) {
    if (!msg || msg.version !== 1 || msg.type !== 'playhead') return;
    latestMessage = msg;
    activeSource = msg.source_file ? normalizedPath(msg.source_file) : null;

    const editor = vscode.window.activeTextEditor;
    if (!editor || !activeSource || normalizedPath(editor.document.uri.fsPath) !== activeSource) {
        clearHighlight();
        if (status) status.hide();
        return;
    }

    const parts = Array.isArray(msg.parts) && msg.parts.length
        ? msg.parts.join(', ')
        : '—';
    const notes = Array.isArray(msg.active_notes) ? msg.active_notes : [];
    const sourceLines = [...new Set(notes.flatMap(note =>
        Array.isArray(note.source_lines) ? note.source_lines : []
    ))].filter(line => Number.isInteger(line) && line > 0 && line <= editor.document.lineCount);
    const decorationKey = `${editor.document.uri.toString()}:${sourceLines.join(',')}`;
    if (decorationKey !== lastDecorationKey || decoratedEditor !== editor) {
        clearHighlight();
        const ranges = sourceLines.map(line => editor.document.lineAt(line - 1).range);
        editor.setDecorations(noteDecoration, ranges);
        decoratedEditor = editor;
        lastDecorationKey = decorationKey;
        if (ranges.length) {
            editor.revealRange(ranges[0], vscode.TextEditorRevealType.InCenterIfOutsideViewport);
        }
    }
    const seconds = Number.isFinite(msg.time_seconds) ? msg.time_seconds : 0;
    const minutes = Math.floor(seconds / 60);
    const remainder = (seconds % 60).toFixed(1).padStart(4, '0');
    status.text = `$(play) Bar ${(msg.arrangement_line || 0) + 1}  ${minutes}:${remainder}  ${parts}  ·  ${notes.length} notes`;
    const noteDetails = notes.map(note => {
        const pitch = Number.isFinite(note.pitch_hz) ? `${note.pitch_hz.toFixed(1)} Hz` : 'unpitched';
        const volume = Number.isFinite(note.volume) ? ` · vol ${note.volume.toFixed(2)}` : '';
        return `${note.part}: ${pitch}${volume}`;
    }).join('\n');
    status.tooltip = `Circolate playback · ${activeSource}${noteDetails ? `\n${noteDetails}` : ''}`;
    status.show();
}

function connect(socketPath) {
    requestedSocketPath = socketPath;
    if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
    }
    if (client) client.destroy();
    buffer = '';
    const connection = net.createConnection({ path: socketPath });
    client = connection;

    connection.on('connect', () => {
        connectionWarningShown = false;
    });

    connection.on('data', data => {
        buffer += data.toString('utf8');
        // Protect the extension host if a malformed or incompatible server floods it.
        if (buffer.length > 1024 * 1024) buffer = buffer.slice(-65536);

        let newline;
        while ((newline = buffer.indexOf('\n')) !== -1) {
            const line = buffer.slice(0, newline);
            buffer = buffer.slice(newline + 1);
            try { render(JSON.parse(line)); } catch (_) { /* Ignore malformed frames. */ }
        }
    });

    connection.on('error', err => {
        if (client !== connection) return;
        if (!connectionWarningShown) {
            vscode.window.showInformationMessage(`Waiting for Circolate playback socket; retrying connection.`);
            connectionWarningShown = true;
        }
        client = null;
        latestMessage = null;
        activeSource = null;
        clearHighlight();
        if (status) status.hide();
        scheduleReconnect();
    });

    connection.on('close', () => {
        if (client !== connection) return;
        client = null;
        activeSource = null;
        latestMessage = null;
        clearHighlight();
        if (status) status.hide();
        scheduleReconnect();
    });
}

function scheduleReconnect() {
    if (!requestedSocketPath || reconnectTimer) return;
    reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        connect(requestedSocketPath);
    }, 1000);
}

function activate(context) {
    noteDecoration = vscode.window.createTextEditorDecorationType({
        isWholeLine: true,
        backgroundColor: 'rgba(255, 190, 80, 0.16)',
        borderColor: 'rgba(255, 190, 80, 0.8)',
        borderWidth: '0 0 0 3px',
        borderStyle: 'solid'
    });
    context.subscriptions.push(noteDecoration);
    status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 10);
    context.subscriptions.push(status);
    context.subscriptions.push(vscode.window.onDidChangeActiveTextEditor(() => {
        if (latestMessage) render(latestMessage);
    }));
    context.subscriptions.push(vscode.commands.registerCommand('halotext.connectPlayhead', () => {
        connect('/tmp/circolate.sock');
    }));
    // Start listening when a Halotext document opens; reconnect automatically
    // if Circolate starts playback after the editor.
    connect('/tmp/circolate.sock');
}

function deactivate() {
    requestedSocketPath = null;
    if (reconnectTimer) clearTimeout(reconnectTimer);
    reconnectTimer = null;
    const connection = client;
    client = null;
    if (connection) connection.destroy();
    clearHighlight();
    if (status) status.dispose();
}

module.exports = { activate, deactivate };
