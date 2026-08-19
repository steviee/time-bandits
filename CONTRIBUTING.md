<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Contributing

## Getting set up

```sh
mise install     # pinned Rust, Node, pnpm, just
just check       # what CI runs: fmt, clippy -D warnings, tests
```

## Ground rules

**Never test enforcement on a machine you depend on.** Use a VM or a throwaway
container. `tests/` contains the container recipes.

**Anything touching `crates/tb-pam` or `/etc/pam.d` needs a test.** A regression
there does not produce a bug report; it produces a family that cannot log in.
The existing decision-table tests in `crates/tb-pam/src/decide.rs` are the
pattern to follow.

**Enforcement logic belongs in `crates/tb-core`**, which has no I/O and no clock
of its own. If a rule cannot be expressed as a pure function over a policy, a
usage snapshot and a timestamp, that is usually a sign the design is wrong
rather than that the rule is special.

**Fail-safe direction is not a matter of taste.** An adversarial condition (the
daemon unreachable) fails closed for managed users. One of our own bugs fails
open. Changing either direction needs discussion in an issue first.

## Language

Code, comments, commit messages and documentation are in English so the project
stays reviewable by distribution maintainers and contributors. User-facing
strings go through i18n; translations are very welcome.

## Licensing

GPL-3.0-or-later, with [REUSE](https://reuse.software) headers on every file. CI
enforces this. No CLA — contributions stay under the project licence.
