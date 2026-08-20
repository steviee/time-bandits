/*
    SPDX-FileCopyrightText: 2026 Time Bandits contributors
    SPDX-License-Identifier: GPL-3.0-or-later
*/

// Reports the focused window to the Time Bandits session agent.
//
// This is the entire desktop-specific surface of the project. Wayland
// deliberately gives clients no way to observe each other's windows, so the
// answer has to come from inside the compositor — a KWin script here, a GNOME
// Shell extension later. Everything upstream of it, from working out which
// application a window belongs to through to the reports a parent reads, is
// shared.
//
// It reports nothing but identity. The agent decides whether the window title
// is kept, based on a policy only the daemon knows, and drops it otherwise.
//
// Watch it work:
//     journalctl --user -b -f | grep timebandits-focus

const SERVICE = "org.timebandits.Agent1";
const PATH = "/org/timebandits/Agent1";
const IFACE = "org.timebandits.Agent1";

function log(message) {
    console.info("timebandits-focus: " + message);
}

/**
 * Logs which application took focus — never the window title.
 *
 * The journal is not a private place, and a caption is exactly what the
 * privacy screen promises is not collected unless a parent asks for it.
 * An identifier is enough to tell whether reporting works.
 */
function trace(window) {
    log("focus: " + (window.desktopFileName || window.resourceClass || "unknown"));
}

/**
 * Sends the focused window's identity to the agent.
 *
 * Empty strings stand for "not known", which is ordinary: plenty of windows
 * carry no desktop file name, and the agent falls back to the window class.
 *
 * @param {object} window The newly activated window, or null when focus was
 *                        lost — a click on the desktop, or the last window
 *                        closing.
 */
function report(window) {
    if (!window) {
        // Focus went nowhere. Say so rather than leaving the agent holding a
        // window that is no longer in front.
        callDBus(SERVICE, PATH, IFACE, "ReportFocus", "", "", "");
        return;
    }

    // Normal windows only. A tooltip or a popup menu taking focus for a moment
    // is not the child switching applications, and counting it would scatter
    // their afternoon across a dozen phantom entries.
    if (!window.normalWindow) {
        return;
    }

    // The caption goes to the agent, which drops it unless the policy says to
    // keep it. It deliberately does not go to the log.
    trace(window);
    callDBus(SERVICE, PATH, IFACE, "ReportFocus",
             window.desktopFileName || "",
             window.resourceClass || "",
             window.caption || "");
}

workspace.windowActivated.connect(report);

// Also report on the way in, so a session that starts with a window already
// focused is not blind until the child switches for the first time.
report(workspace.activeWindow);

log("reporting focus to " + SERVICE);
