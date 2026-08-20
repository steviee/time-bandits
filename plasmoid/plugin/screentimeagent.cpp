/*
 * SPDX-FileCopyrightText: 2026 Time Bandits contributors
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "screentimeagent.h"

#include <QDBusConnection>
#include <QDBusInterface>
#include <QDBusMessage>
#include <QDBusMetaType>
#include <QDBusReply>
#include <QLoggingCategory>

namespace
{
constexpr auto Service = "org.timebandits.Agent1";
constexpr auto Path = "/org/timebandits/Agent1";
constexpr auto Interface = "org.timebandits.Agent1";

Q_LOGGING_CATEGORY(LOG, "org.timebandits.screentime")

/** One application's share of the day, as the agent sends it: (id, name, secs). */
using AppShare = struct {
    QString id;
    QString name;
    uint secs;
};

/** One day of the week: (weekday, allowance, used, today). */
using DayShare = struct {
    QString weekday;
    qlonglong allowance;
    uint used;
    bool today;
};
}

ScreenTimeAgent::ScreenTimeAgent(QObject *parent)
    : QObject(parent)
    , m_watcher(QString::fromLatin1(Service),
                QDBusConnection::sessionBus(),
                QDBusServiceWatcher::WatchForOwnerChange)
{
    connect(&m_watcher, &QDBusServiceWatcher::serviceOwnerChanged,
            this, &ScreenTimeAgent::onAgentOwnerChanged);

    // Follow the agent's own announcements rather than polling. A panel widget
    // that wakes on a timer costs battery for nothing most of the time.
    QDBusConnection::sessionBus().connect(QString::fromLatin1(Service),
                                          QString::fromLatin1(Path),
                                          QStringLiteral("org.freedesktop.DBus.Properties"),
                                          QStringLiteral("PropertiesChanged"),
                                          this,
                                          SLOT(onPropertiesChanged()));
    refresh();
}

void ScreenTimeAgent::onAgentOwnerChanged(const QString &, const QString &, const QString &newOwner)
{
    if (newOwner.isEmpty()) {
        // The agent went away. Keep the last figures but stop claiming they
        // describe now.
        m_available = false;
        Q_EMIT changed();
        return;
    }
    refresh();
}

void ScreenTimeAgent::onPropertiesChanged()
{
    refresh();
}

void ScreenTimeAgent::refresh()
{
    QDBusInterface properties(QString::fromLatin1(Service),
                              QString::fromLatin1(Path),
                              QStringLiteral("org.freedesktop.DBus.Properties"),
                              QDBusConnection::sessionBus());
    if (!properties.isValid()) {
        m_available = false;
        Q_EMIT changed();
        return;
    }

    const QDBusReply<QVariantMap> reply =
        properties.call(QStringLiteral("GetAll"), QString::fromLatin1(Interface));
    if (!reply.isValid()) {
        qCDebug(LOG) << "cannot read agent properties:" << reply.error().message();
        m_available = false;
        Q_EMIT changed();
        return;
    }

    m_values = reply.value();
    m_available = m_values.value(QStringLiteral("Available")).toBool();
    Q_EMIT changed();
}

bool ScreenTimeAgent::available() const
{
    return m_available;
}

QString ScreenTimeAgent::subject() const
{
    return m_values.value(QStringLiteral("Subject")).toString();
}

bool ScreenTimeAgent::enforcement() const
{
    return m_values.value(QStringLiteral("Enforcement")).toBool();
}

bool ScreenTimeAgent::blocked() const
{
    return m_values.value(QStringLiteral("Blocked")).toBool();
}

qlonglong ScreenTimeAgent::remainingSeconds() const
{
    // -1 rather than 0 when absent: an unread property must not read as "your
    // time is up".
    return m_values.value(QStringLiteral("RemainingSeconds"), -1).toLongLong();
}

uint ScreenTimeAgent::usedTodaySeconds() const
{
    return m_values.value(QStringLiteral("UsedTodaySeconds")).toUInt();
}

QString ScreenTimeAgent::message() const
{
    return m_values.value(QStringLiteral("Message")).toString();
}

QString ScreenTimeAgent::retryClock() const
{
    return m_values.value(QStringLiteral("RetryClock")).toString();
}

bool ScreenTimeAgent::recordTitles() const
{
    return m_values.value(QStringLiteral("RecordTitles")).toBool();
}

bool ScreenTimeAgent::focusTracking() const
{
    return m_values.value(QStringLiteral("FocusTracking")).toBool();
}

QString ScreenTimeAgent::budgetKind() const
{
    return m_values.value(QStringLiteral("BudgetKind"), QStringLiteral("daily")).toString();
}

qlonglong ScreenTimeAgent::weeklyRemainingSeconds() const
{
    return m_values.value(QStringLiteral("WeeklyRemainingSeconds"), -1).toLongLong();
}

QVariantList ScreenTimeAgent::apps() const
{
    QVariantList out;
    const QDBusArgument argument = m_values.value(QStringLiteral("Apps")).value<QDBusArgument>();
    if (argument.currentType() != QDBusArgument::ArrayType) {
        return out;
    }
    argument.beginArray();
    while (!argument.atEnd()) {
        AppShare app;
        argument.beginStructure();
        argument >> app.id >> app.name >> app.secs;
        argument.endStructure();
        out.append(QVariantMap{
            {QStringLiteral("id"), app.id},
            {QStringLiteral("name"), app.name},
            {QStringLiteral("seconds"), app.secs},
        });
    }
    argument.endArray();
    return out;
}

QVariantList ScreenTimeAgent::week() const
{
    QVariantList out;
    const QDBusArgument argument = m_values.value(QStringLiteral("Week")).value<QDBusArgument>();
    if (argument.currentType() != QDBusArgument::ArrayType) {
        return out;
    }
    argument.beginArray();
    while (!argument.atEnd()) {
        DayShare day;
        argument.beginStructure();
        argument >> day.weekday >> day.allowance >> day.used >> day.today;
        argument.endStructure();
        out.append(QVariantMap{
            {QStringLiteral("weekday"), day.weekday},
            // -1 means there is nothing to state — unlimited, or a weekly pot
            // with no per-day share. Zero means no computer that day.
            {QStringLiteral("allowanceSeconds"), day.allowance},
            {QStringLiteral("usedSeconds"), day.used},
            {QStringLiteral("today"), day.today},
        });
    }
    argument.endArray();
    return out;
}
