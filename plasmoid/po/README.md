<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Translations

The widget's own strings. Everything the child reads *from the daemon* — the
refusal on the lock screen, the notification when time runs out — is translated
on the Rust side in `crates/tb-proto/src/text.rs`, because only that side knows
the locale of the session being spoken to.

## Adding a language

```sh
msginit --no-wrap --locale=fr --input=plasma_applet_org.timebandits.screentime.pot --output=fr.po
# translate fr.po
```

Then add it to `LANGUAGES` in the top-level `Makefile`. Anything not translated
falls back to the source strings, which are English.

## After changing a string in the QML

```sh
make po-update
```

This regenerates the template and folds the changes into every language,
keeping what is already translated.

## Two things worth knowing

`--no-wrap` is not cosmetic. gettext splits long strings across lines by
default, and a split `msgid` is one that tooling quietly fails to match — the
first sign is a single stubbornly untranslated entry with no obvious cause.

The catalog is named `plasma_applet_<plugin id>` and installs to
`share/locale/<lang>/LC_MESSAGES/`, not into the plasmoid package. That is
where Plasma looks; a `.mo` inside `contents/` is simply ignored.
