<!--
SPDX-FileCopyrightText: 2026 Time Bandits contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Trying it in a virtual machine

**Use a virtual machine.** Not a spare account on your own laptop, not a second
user on the family PC. Step 5 edits the PAM stack, and getting it wrong locks
people out of the machine.

Tested against Fedora 44 KDE and Bazzite. Any Plasma 6 system with systemd works.

## 1. Build and install

```sh
mise install                       # or: dnf install cargo rust make gcc pam-devel sqlite-devel
just check                         # optional, but it tells you the machine is sane
sudo make install                  # daemon, PAM module, tbctl, agent, units, groups
sudo make install-plasma           # the widget: needs cmake and qt6-declarative-devel
```

`install-plasma` is separate on purpose. The enforcing half needs no desktop at
all — it works on GNOME or Sway just as well — and only the widget and the focus
reporter are KDE-specific.

## 2. Create the accounts

The groups come from the package; the membership is yours to decide.

```sh
sudo useradd -m -G kids alice
sudo passwd alice
sudo usermod -aG parents "$USER"    # yourself, so you are never locked out
```

Log out and back in for your own group change to take effect.

## 3. Start the daemon

```sh
sudo systemctl enable --now timebanditsd
systemctl status timebanditsd
```

## 4. Give Alice some rules

Nothing is limited until a policy exists. Start in observing mode — it records
without restricting, which is the honest way to find out what a week actually
looks like before deciding what to allow:

```sh
sudo tbctl policy set alice --timezone Europe/Berlin \
      --daily 2h --daily 'sat=3h' --daily 'sun=3h' \
      --window 'mon=15:00-20:00' --window 'tue=15:00-20:00' \
      --window 'wed=15:00-20:00' --window 'thu=15:00-20:00' \
      --window 'fri=15:00-20:00' \
      --window 'sat=10:00-20:00' --window 'sun=10:00-20:00'

sudo tbctl policy show alice
```

Or the weekly arrangement, where Alice divides the time herself:

```sh
sudo tbctl policy set alice --daily unlimited --weekly 14h --timezone Europe/Berlin
```

Switch enforcement on when the rules look right:

```sh
sudo tbctl policy set alice --enforcement true
```

## 5. Wire up PAM

**Read [pam-setup.md](pam-setup.md) first.** This is the step that can lock
people out.

```sh
sudo tbctl pam enable --dry-run     # shows every change without making one
sudo tbctl pam enable
sudo tbctl doctor                   # will this installation actually enforce anything?
```

Every file is backed up before its first edit, and `sudo tbctl pam disable`
restores them. `root` and everyone in `parents` are exempt before any of this
logic runs, so a text console remains a way back in.

## 6. Log in as Alice and turn on the session pieces

```sh
systemctl --user enable --now timebandits-agent

kwriteconfig6 --file kwinrc --group Plugins --key org.timebandits.focusEnabled true
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.start
```

Then add **Screen Time** to the panel: right-click the panel, *Add Widgets*.

## What you should see

| | |
|---|---|
| Panel | A countdown, e.g. `1:12`, in blue |
| Popup | A ring, today's applications, and the week |
| At 15 and 5 minutes | A desktop notification |
| At zero | The screen locks |
| Unlocking | Refused, with the time it comes back |
| `sudo tbctl grant-bonus alice 15m` | Alice is back in within seconds |

## When something looks wrong

```sh
sudo tbctl doctor                                # the first thing to run
sudo tbctl status                                # everyone, at a glance
journalctl -u timebanditsd -f                    # the daemon
journalctl --user -u timebandits-agent -f        # the agent
journalctl --user -b -f | grep timebandits-focus # the KWin script
```

Rules for a child are `/etc/timebandits/policy.d/<user>.toml`; `tbctl policy
path <user>` prints the location. Edit the file directly if you like — the
daemon picks it up within a few seconds.

**Nothing is enforced until the user is in `kids`.** A policy on its own only
records; the group membership is what authorises locking. If `tbctl doctor`
looks healthy and nothing ever locks, check `id -nG alice` first.

Three things that look like bugs and are not:

- **No time is counted while the screen is locked.** That is the point, and it
  also means a session you left locked records nothing.
- **`Scripting.start()` will not reload a KWin script that is already running.**
  Use `qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript
  org.timebandits.focus` first.
- **Time shows up under "Something else".** The focus reporter is not running,
  or the agent is not. Time still counts; only the attribution is missing.

## Getting back out

```sh
sudo tbctl pam disable
sudo systemctl disable --now timebanditsd
sudo touch /etc/timebandits/disable    # the emergency brake: works even if the
                                       # daemon cannot be reached at all
```
