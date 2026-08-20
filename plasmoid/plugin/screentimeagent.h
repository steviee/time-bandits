/*
 * SPDX-FileCopyrightText: 2026 Time Bandits contributors
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma once

#include <QDBusServiceWatcher>
#include <QObject>
#include <QQmlEngine>
#include <QVariantList>

/**
 * The widget's view of the session agent.
 *
 * Pure glue: it mirrors the properties of org.timebandits.Agent1 into QML and
 * turns PropertiesChanged into QML signals. It decides nothing and formats
 * nothing — the agent composes the sentences, in the locale of the session it
 * runs in, which is the only place that locale is known.
 *
 * This exists because Plasma 6 gives QML no way to speak D-Bus, and because no
 * data engines are installed on a modern Plasma system. Every shipped KDE
 * widget that needs external data carries a plugin like this one.
 */
class ScreenTimeAgent : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON

    /** Whether the agent is on the bus and has heard from the daemon. */
    Q_PROPERTY(bool available READ available NOTIFY changed)
    Q_PROPERTY(QString subject READ subject NOTIFY changed)
    /** Whether anything is actually being limited. */
    Q_PROPERTY(bool enforcement READ enforcement NOTIFY changed)
    Q_PROPERTY(bool blocked READ blocked NOTIFY changed)
    /** Seconds left, or -1 for unlimited. Signed so "no limit" and "no time"
     *  cannot collapse into the same number. */
    Q_PROPERTY(qlonglong remainingSeconds READ remainingSeconds NOTIFY changed)
    Q_PROPERTY(uint usedTodaySeconds READ usedTodaySeconds NOTIFY changed)
    /** The refusal, already in this session's language. */
    Q_PROPERTY(QString message READ message NOTIFY changed)
    /** Wall-clock time access returns, "HH:MM", empty when not blocked. */
    Q_PROPERTY(QString retryClock READ retryClock NOTIFY changed)
    Q_PROPERTY(bool recordTitles READ recordTitles NOTIFY changed)
    /** False means the breakdown is incomplete and the widget should say so. */
    Q_PROPERTY(bool focusTracking READ focusTracking NOTIFY changed)
    /** "daily" or "weekly". */
    Q_PROPERTY(QString budgetKind READ budgetKind NOTIFY changed)
    /** Seconds left in the week, or -1 when there is no weekly budget. */
    Q_PROPERTY(qlonglong weeklyRemainingSeconds READ weeklyRemainingSeconds NOTIFY changed)
    /** [{id, name, seconds}], longest first. */
    Q_PROPERTY(QVariantList apps READ apps NOTIFY changed)
    /** [{weekday, allowanceSeconds, usedSeconds, today}], Monday first.
     *  allowanceSeconds is -1 when there is nothing to state. */
    Q_PROPERTY(QVariantList week READ week NOTIFY changed)

public:
    explicit ScreenTimeAgent(QObject *parent = nullptr);

    bool available() const;
    QString subject() const;
    bool enforcement() const;
    bool blocked() const;
    qlonglong remainingSeconds() const;
    uint usedTodaySeconds() const;
    QString message() const;
    QString retryClock() const;
    bool recordTitles() const;
    bool focusTracking() const;
    QString budgetKind() const;
    qlonglong weeklyRemainingSeconds() const;
    QVariantList apps() const;
    QVariantList week() const;

Q_SIGNALS:
    /**
     * Something changed.
     *
     * One signal for every property rather than fifteen: the widget re-reads
     * what it shows, and the agent announces in one batch anyway.
     */
    void changed();

private Q_SLOTS:
    void onPropertiesChanged();
    void onAgentOwnerChanged(const QString &service, const QString &oldOwner, const QString &newOwner);

private:
    void refresh();

    /** Everything the last refresh read, so a property access never blocks on
     *  the bus. An agent that has gone away leaves the last known values, and
     *  m_available goes false — a stale countdown is worse than none. */
    QVariantMap m_values;
    bool m_available = false;
    QDBusServiceWatcher m_watcher;
};
