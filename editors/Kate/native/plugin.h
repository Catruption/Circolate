#pragma once

#include <KTextEditor/Attribute>
#include <KTextEditor/Document>
#include <KTextEditor/MainWindow>
#include <KTextEditor/MovingRange>
#include <KTextEditor/Plugin>
#include <KTextEditor/View>

#include <QJsonObject>
#include <QJsonArray>
#include <QLocalSocket>
#include <QObject>
#include <QPointer>
#include <QTimer>

#include <vector>

class HalotextPlayheadPluginView final : public QObject {
    Q_OBJECT

public:
    explicit HalotextPlayheadPluginView(KTextEditor::MainWindow *mainWindow);
    ~HalotextPlayheadPluginView() override;

private:
    void connectSocket();
    void readSocket();
    void handleMessage(const QJsonObject &message);
    void setActiveView(KTextEditor::View *view);
    void updateHighlight(const QJsonObject &message);
    void clearHighlight();
    void scheduleReconnect();

    QPointer<KTextEditor::MainWindow> m_mainWindow;
    QPointer<KTextEditor::View> m_view;
    QPointer<KTextEditor::Document> m_document;
    QLocalSocket m_socket;
    QTimer m_reconnectTimer;
    QByteArray m_buffer;
    QJsonObject m_latestMessage;
    QByteArray m_highlightSignature;
    bool m_loggedFirstMessage = false;
    std::vector<KTextEditor::MovingRange *> m_ranges;
    KTextEditor::Attribute::Ptr m_attribute;
};

class HalotextPlayheadPlugin final : public KTextEditor::Plugin {
    Q_OBJECT

public:
    explicit HalotextPlayheadPlugin(QObject *parent,
                                    const QList<QVariant> &arguments = {});
    QObject *createView(KTextEditor::MainWindow *mainWindow) override;
};
