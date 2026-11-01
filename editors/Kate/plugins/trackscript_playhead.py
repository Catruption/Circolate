import json
import os
import kate
from PyKDE4.kdecore import i18n
from PyKDE4.ktexteditor import KTextEditor
from PyQt4.QtNetwork import QLocalSocket
from PyQt4 import QtGui

class HalotextPlayhead(kate.KatePlugin):
    def __init__(self, parent=None):
        super(HalotextPlayhead, self).__init__(parent)
        self.socket = QLocalSocket()
        self.current_view = None
        self.moving_ranges = []
        self.attr = None
        self.buffer = ''

    def connect_to_circolate(self, view):
        self.current_view = view
        self.socket.connectToServer("/tmp/circolate.sock")
        if self.socket.waitForConnected(1000):
            self.socket.readyRead.connect(self.on_data_received)
            view.document().setModified(False) # Just to ensure doc is ready

    def on_data_received(self):
        self.buffer += self.socket.readAll().data().decode('utf-8')
        doc = self.current_view.document() if self.current_view else None
        if not doc:
            return
        try:
            current_path = os.path.realpath(str(doc.url().toLocalFile()))
        except Exception:
            current_path = ''

        while '\n' in self.buffer:
            line, self.buffer = self.buffer.split('\n', 1)
            if not line:
                continue
            try:
                msg = json.loads(line)
                if msg.get('version') != 1 or msg.get('type') != 'playhead':
                    continue
                if os.path.realpath(msg.get('source_file', '')) != current_path:
                    self.highlight_lines([])
                    continue
                source_lines = set()
                for note in msg.get('active_notes', []):
                    source_lines.update(note.get('source_lines', []))
                self.highlight_lines(sorted(source_lines))
            except (ValueError, TypeError, KeyError):
                continue

    def highlight_lines(self, line_nums):
        if not self.current_view:
            return
        doc = self.current_view.document()
        for moving_range in self.moving_ranges:
            self.current_view.removeRange(moving_range)
        self.moving_ranges = []

        if not self.attr:
            self.attr = KTextEditor.Attribute()
            self.attr.setBackground(QtGui.QColor(255, 255, 255, 20)) # Subtle white
            self.attr.setForeground(QtGui.QColor(100, 200, 255))     # Cyan text
            self.attr.setFontBold(True)

        for source_line in line_nums:
            # IPC source lines are one-based; Kate cursors are zero-based.
            line_num = int(source_line) - 1
            if line_num < 0 or line_num >= doc.lines():
                continue
            start = KTextEditor.Cursor(line_num, 0)
            end = KTextEditor.Cursor(line_num, doc.lineLength(line_num))
            moving_range = self.current_view.newMovingRange(start, end)
            moving_range.setAttribute(self.attr)
            self.moving_ranges.append(moving_range)

@kate.init
def init():
    pass

@kate.action(i18n("Connect to Circolate Playhead"), shortcut="Ctrl+Shift+P")
def connect_playhead():
    view = kate.activeView()
    if view:
        # Store the plugin instance on the view so it doesn't get garbage collected
        if not hasattr(view, '_halotext_playhead'):
            view._halotext_playhead = HalotextPlayhead()
        view._halotext_playhead.connect_to_circolate(view)
