<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Decisions

Settled questions, with the reasoning that settled them. A decision that lives
only in a conversation gets re-litigated; this file is where they stop being
open.

---

## The child sees the week, not just the day

**Decided.** The widget shows the week: what has been used, and what each of the
coming days holds.

With a daily budget those figures are facts — each day carries its own
allowance. With a weekly budget they are not, because there is no per-day
allocation; the strip then shows the week's remainder and, separately, what it
works out to per remaining day, labelled as the arithmetic it is.

A child can only divide up a week they can see. Showing a single number would
make self-allocation impossible and the Sunday lockout a surprise.

## A weekly budget can replace daily limits

**Decided, and it needed no new mechanism.** Unlimited days plus a weekly
ceiling already means the child allocates the week themselves; a daily ceiling
alongside a weekly budget already binds first and says which rule refused.

The research supports it more strongly than expected: the American Academy of
Pediatrics **retired its hourly screen-limit framework in January 2026** in
favour of contextual, autonomy-supporting approaches, and work on
self-regulation finds strict limits undermine the thing they are meant to
teach.

Three arrangements are therefore supported, and all three are tested:

| Arrangement | Policy |
|---|---|
| Two hours a day, only between three and eight | daily quota + windows |
| Fourteen hours a week, same daily frame | unlimited days + weekly quota + windows |
| Free within the week, never more than three hours in a day | daily quota + weekly quota + windows |

Budget and frame are independent dimensions. Either budget works with any set of
windows, and every weekday can carry its own amount *and* its own hours.

## Asking for more time is attached to the warnings

**Decided.** The request is offered where the child already is: on the warning
that time is running low, and on the notification when it runs out.

KDE's notification service reports `actions` and `inline-reply` among its
capabilities, so the warning can carry a button and the child can add a reason
without leaving what they are doing.

**The lock screen cannot carry a button.** KScreenLocker renders text from the
PAM conversation and nothing else — a hard limit, not a matter of effort. What
it can do is say that a request is already waiting.

So the request has to be possible *before* the lock, and the refused unlock
itself becomes a signal: the first attempt after the quota expires is reported
to the parents as an implicit request, rate-limited so it is not spam.

## A removed widget comes back at the next login

**Decided.** `org.kde.PlasmaShell.evaluateScript` needs no authorisation, so the
agent can put the widget back when it is missing at session start.

Once per session, not in a loop. What the agent can add, the child can remove
with the same call — making a contest of it teaches that the computer is an
opponent, and wins nothing. The loss is smaller than it looks: warnings are
desktop notifications and do not depend on the widget being in the panel.

Restoration is recorded as an event a parent can see.

## German and English from the first release

**Decided.** Both ship at launch, chosen by locale, with anything else falling
back to English rather than failing.

This forced an architectural correction. The daemon was composing the text a
child reads and sending it finished, on the stated grounds that it "knows the
configured locale" — it does not. It runs as a systemd service, usually with no
`LANG` at all and never the child's, so every refusal would have arrived in
English on a German desktop.

The split is now facts from the daemon, prose from each front end: *why* access
was refused and the wall-clock time it returns, since only the daemon knows the
policy's time zone. The lock screen writes its sentence in the login
environment's locale, which for an unlock is the child's own session.

## The Plasma plugin is C++, everything else is Rust

**Decided, after the first recommendation turned out to rest on a false premise.**

Plasma 6 gives QML no way to speak D-Bus, and no data engines are installed at
all — every shipped KDE widget with external data carries a compiled C++ plugin.
Two dead ends were ruled out by testing rather than reading: Qt 6 blocks
`XMLHttpRequest` on local files without `QML_XHR_ALLOW_FILE_READ=1`, silently,
and cxx-qt's Cargo-only path produces a module for linking into your own Qt
application, not a loadable plugin — Qt rejects it with "is not a Qt plugin".

CMake is therefore unavoidable either way. Given that, the plugin is C++: it is
roughly a hundred lines of QtDBus glue, it adds no Rust toolchain to the Plasma
package, and it is the shape distribution packagers expect. The logic stays in
Rust, where it is.

---

## Rules are files, usage is a database

Storage is split by what the data is. Rules are configuration: a handful of
short documents a parent should be able to `cat`, edit, keep in a backup and
still understand in three years. Usage is data: append-heavy, queried by time
range, growing without bound. So rules live as one TOML file per child in
`/etc/timebandits/policy.d/`, and usage lives in SQLite.

They were briefly JSON blobs inside SQLite, which is the worst of both — not
queryable as structure, not readable as configuration. An older installation
moves its policies into files on first start.

Two things fall out of the choice:

- There is no cache in front of the files. A policy is under a kilobyte, so the
  tick loop simply reads it, and a hand edit takes effect on the next pass with
  no watcher and no reload command.
- A hand edit counts as current. A policy arriving from the hub has to carry a
  higher version to replace what somebody typed into the file.

## Still open

**Whether a reason is required when asking for more time.** Optional is the
current plan, since `inline-reply` makes typing one cheap without making it a
precondition. Not blocking anything.
