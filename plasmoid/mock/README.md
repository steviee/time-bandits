<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Widget design harness

Every state of the plasmoid side by side, in one window:

```sh
qml6 plasmoid/mock/main.qml
```

It renders with the real Plasma components and the system colour scheme, so
what appears is what Plasma will draw — no approximation, and no need to install
anything into a live panel first.

## Why this exists separately from the plasmoid

The shipped widget's root is a `PlasmoidItem`, which only resolves inside a
Plasma shell. That makes the widget awkward to look at while designing it: the
loop is install, restart plasmashell, click the panel, and repeat. Everything
below the root is ordinary `PlasmaComponents` and `Kirigami`, so it can be
rendered in a plain `Window` instead, which is what this harness does.

`plasmoidviewer` from `plasma-sdk` does the same job for a real plasmoid package
and is worth having once the widget exists; it is not installed on every machine,
and this harness needs nothing beyond Qt and the Plasma QML modules.

## Files

| File | Contents |
|---|---|
| `main.qml` | The harness window: panel chips and all four popup states |
| `PopupMock.qml` | The popup itself, switched by `mode` |
| `TimeRing.qml` | The remaining-time ring |
| `AppRow.qml` | One application's share of the day |

Strings go through an `i18nMock()` stand-in for the real `i18n()`, which only
exists inside a plasmoid. Keeping every string wrapped means the switch to real
translation is a find-and-replace rather than a rewrite.
