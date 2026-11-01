#include "plugin.h"

#include <KPluginFactory>
#include <KTextEditor/Attribute>
#include <KTextEditor/Document>
#include <KTextEditor/MainWindow>
#include <KTextEditor/MovingRange>
#include <KTextEditor/Range>
#include <KTextEditor/View>

#include <QColor>
#include <QDir>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonValue>
#include <QLoggingCategory>
#include <QUrl>

#include <algorithm>
#include <cmath>
#include <utility>

K_PLUGIN_FACTORY_WITH_JSON(HalotextPlayheadFactory, "plugin.json",
                           registerPlugin<HalotextPlayheadPlugin>();)

Q_LOGGING_CATEGORY(logHalotextPlayhead, "halotext.playhead")

namespace {
QString normalizedPath(const QString &path)
{
    if (path.isEmpty()) {
        return {};
    }
    const QFileInfo info(path);
    const QString canonical = info.canonicalFilePath();
    return QDir::cleanPath(canonical.isEmpty() ? info.absoluteFilePath() : canonical);
}
}

HalotextPlayheadPlugin::HalotextPlayheadPlugin(QObject *parent,
                                               const QList<QVariant> &arguments)
    : KTextEditor::Plugin(parent)
{
    Q_UNUSED(arguments);
}

QObject *HalotextPlayheadPlugin::createView(KTextEditor::MainWindow *mainWindow)
{
    return new HalotextPlayheadPluginView(mainWindow);
}

HalotextPlayheadPluginView::HalotextPlayheadPluginView(KTextEditor::MainWindow *mainWindow)
    : QObject(mainWindow)
    , m_mainWindow(mainWindow)
{
    m_attribute = KTextEditor::Attribute::Ptr(new KTextEditor::Attribute);
    m_attribute->setBackground(QColor(255, 190, 80, 42));
    m_attribute->setForeground(QColor(255, 220, 150));
    m_attribute->setFontBold(true);

    m_reconnectTimer.setInterval(1000);
    connect(&m_reconnectTimer, &QTimer::timeout, this,
            &HalotextPlayheadPluginView::connectSocket);
    connect(&m_socket, &QLocalSocket::readyRead, this,
            &HalotextPlayheadPluginView::readSocket);
    connect(&m_socket, &QLocalSocket::connected, this, [this] {
        m_buffer.clear();
        qCDebug(logHalotextPlayhead) << "connected to /tmp/circolate.sock";
    });
    connect(&m_socket, &QLocalSocket::disconnected, this, [this] {
        clearHighlight();
        scheduleReconnect();
    });
    connect(&m_socket, &QLocalSocket::errorOccurred, this,
            [this](QLocalSocket::LocalSocketError) {
        qCDebug(logHalotextPlayhead) << "socket error:" << m_socket.errorString();
        if (m_socket.state() == QLocalSocket::UnconnectedState) {
            scheduleReconnect();
        }
    });

    connect(mainWindow, &KTextEditor::MainWindow::viewChanged,
            this, &HalotextPlayheadPluginView::setActiveView);
    setActiveView(mainWindow->activeView());
    connectSocket();
}

HalotextPlayheadPluginView::~HalotextPlayheadPluginView()
{
    m_reconnectTimer.stop();
    m_socket.abort();
    clearHighlight();
}

void HalotextPlayheadPluginView::connectSocket()
{
    if (m_socket.state() != QLocalSocket::UnconnectedState) {
        return;
    }
    m_socket.connectToServer(QStringLiteral("/tmp/circolate.sock"), QIODevice::ReadOnly);
    if (m_socket.state() != QLocalSocket::UnconnectedState) {
        m_reconnectTimer.stop();
    } else {
        scheduleReconnect();
    }
}

void HalotextPlayheadPluginView::scheduleReconnect()
{
    if (!m_reconnectTimer.isActive()) {
        m_reconnectTimer.start();
    }
}

void HalotextPlayheadPluginView::readSocket()
{
    m_buffer += m_socket.readAll();
    if (m_buffer.size() > 1024 * 1024) {
        m_buffer = m_buffer.right(64 * 1024);
    }

    qsizetype newline = -1;
    while ((newline = m_buffer.indexOf('\n')) >= 0) {
        const QByteArray frame = m_buffer.left(newline);
        m_buffer.remove(0, newline + 1);
        QJsonParseError error;
        const QJsonDocument document = QJsonDocument::fromJson(frame, &error);
        if (error.error == QJsonParseError::NoError && document.isObject()) {
            handleMessage(document.object());
        }
    }
}

void HalotextPlayheadPluginView::handleMessage(const QJsonObject &message)
{
    if (message.value(QStringLiteral("version")).toInt() != 1
        || message.value(QStringLiteral("type")).toString() != QStringLiteral("playhead")) {
        return;
    }

    m_latestMessage = message;
    if (!m_view || !m_document || !m_document->url().isLocalFile()) {
        clearHighlight();
        return;
    }

    const QString source = normalizedPath(
        message.value(QStringLiteral("source_file")).toString());
    const QString openFile = normalizedPath(m_document->url().toLocalFile());
    if (!m_loggedFirstMessage) {
        qCDebug(logHalotextPlayhead) << "first playhead frame; source=" << source
                                     << "document=" << openFile
                                     << "active_notes="
                                     << message.value(QStringLiteral("active_notes")).toArray().size();
        m_loggedFirstMessage = true;
    }
    if (source.isEmpty() || source != openFile) {
        qCDebug(logHalotextPlayhead) << "source file does not match open document";
        clearHighlight();
        return;
    }

    updateHighlight(message);
}

void HalotextPlayheadPluginView::setActiveView(KTextEditor::View *view)
{
    KTextEditor::Document *document = view ? view->document() : nullptr;
    if (m_view == view && m_document == document) {
        return;
    }
    clearHighlight();
    m_view = view;
    m_document = document;
    if (!m_document) {
        return;
    }

    connect(m_document, &QObject::destroyed, this, [this] {
        m_ranges.clear();
        m_view = nullptr;
        m_document = nullptr;
    });
    connect(m_document, &KTextEditor::Document::aboutToInvalidateMovingInterfaceContent,
            this, [this] { clearHighlight(); });
    connect(m_document, &KTextEditor::Document::reloaded, this, [this] {
        if (!m_latestMessage.isEmpty()) {
            handleMessage(m_latestMessage);
        }
    });
    connect(m_document, &KTextEditor::Document::documentUrlChanged, this, [this] {
        if (!m_latestMessage.isEmpty()) {
            handleMessage(m_latestMessage);
        }
    });
    if (!m_latestMessage.isEmpty()) {
        handleMessage(m_latestMessage);
    }
}

void HalotextPlayheadPluginView::updateHighlight(const QJsonObject &message)
{
    QJsonObject signatureData;
    signatureData.insert(QStringLiteral("active_notes"), message.value(QStringLiteral("active_notes")));
    signatureData.insert(QStringLiteral("synesthesia"), message.value(QStringLiteral("synesthesia")));
    const QByteArray signature = QJsonDocument(signatureData).toJson(QJsonDocument::Compact);
    if (signature == m_highlightSignature) {
        return;
    }
    clearHighlight();
    if (!m_view || !m_document) {
        return;
    }

    const QJsonObject synth = message.value(QStringLiteral("synesthesia")).toObject();
    const bool usePitchColors = synth.value(QStringLiteral("enabled")).toBool(true);
    const double hueStep = synth.value(QStringLiteral("hue_step")).toDouble(30.0);
    const int saturation = qBound(0, qRound(synth.value(QStringLiteral("saturation_scale")).toDouble(1.0) * 255.0), 255);
    const double overallValue = synth.value(QStringLiteral("value")).toDouble(1.0);
    const QJsonArray notes = message.value(QStringLiteral("active_notes")).toArray();
    for (const QJsonValue &noteValue : notes) {
        const QJsonObject note = noteValue.toObject();
        const double pitch = note.value(QStringLiteral("pitch_hz")).toDouble(0.0);
        const double volume = qBound(0.0, note.value(QStringLiteral("volume")).toDouble(0.0) / 100.0, 1.0);
        const int midi = pitch > 0.0 ? qRound(60.0 + 12.0 * std::log2(pitch / 523.2511306)) : 60;
        const double brightness = (0.1 + volume * 0.9) * overallValue;
        const int hue = qRound((midi % 12 + 12) % 12 * hueStep) % 360;
        auto attribute = KTextEditor::Attribute::Ptr(new KTextEditor::Attribute);
        if (usePitchColors) {
            attribute->setBackground(QColor::fromHsv(hue, saturation,
                qBound(0, qRound(brightness * 255.0), 255)));
            attribute->setForeground(brightness > 0.6 ? QColor(Qt::black) : QColor(Qt::white));
        } else {
            attribute = m_attribute;
        }
        attribute->setFontBold(true);

        const QJsonArray spans = note.value(QStringLiteral("source_spans")).toArray();
        for (const QJsonValue &spanValue : spans) {
            const QJsonObject span = spanValue.toObject();
            const int line = span.value(QStringLiteral("line")).toInt() - 1;
            const int startByte = span.value(QStringLiteral("start_byte_column")).toInt(-1);
            const int endByte = span.value(QStringLiteral("end_byte_column")).toInt(-1);
            if (line < 0 || line >= m_document->lines() || startByte < 0 || endByte <= startByte) {
                continue;
            }
            const QByteArray lineUtf8 = m_document->line(line).toUtf8();
            if (startByte >= lineUtf8.size()) {
                continue;
            }
            const int start = QString::fromUtf8(lineUtf8.constData(), startByte).size();
            const int end = QString::fromUtf8(
                lineUtf8.constData(), qMin(endByte, lineUtf8.size())).size();
            if (end <= start || start >= m_document->lineLength(line)) {
                continue;
            }
            auto *range = m_document->newMovingRange(
                KTextEditor::Range(line, start, line, qMin(end, m_document->lineLength(line))));
            if (!range) {
                continue;
            }
            range->setView(m_view);
            range->setAttribute(attribute);
            range->setAttributeOnlyForViews(true);
            m_ranges.push_back(range);
        }
    }
    m_highlightSignature = signature;
}

void HalotextPlayheadPluginView::clearHighlight()
{
    for (auto *range : m_ranges) {
        delete range;
    }
    m_ranges.clear();
    m_highlightSignature.clear();
}

#include "plugin.moc"
