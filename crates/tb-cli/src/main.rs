// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `tbctl` — inspect and change what Time Bandits enforces.
//!
//! The tool reads and writes the daemon's database directly and therefore needs
//! root. That is a deliberate interim design: the D-Bus interface that will let
//! a parent do this without `sudo`, gated by polkit, comes with the session
//! agent. Shipping a CLI that only root can use is better than shipping none,
//! because until this exists there is no way to configure anything at all.
//!
//! Changes take effect within one tick — the daemon re-reads the policy and any
//! bonus grants on every pass, so a child locked out a moment ago is back in a
//! few seconds later without anything being restarted.

mod doctor;
mod pamconf;
mod policyedit;
mod report;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Parser, Subcommand};
use jiff::Zoned;
use tb_core::duration::DurationSpec;
use tb_core::policy::{LockAction, Policy, Quota};
use tb_daemon::config::Config;
use tb_daemon::store::Store;

use policyedit::{PolicyEdit, QuotaArg, WindowArg};

#[derive(Parser, Debug)]
#[command(
    name = "tbctl",
    version,
    about = "Inspect and change Time Bandits screen-time rules",
    long_about = None
)]
struct Cli {
    /// Daemon configuration to read paths from.
    #[arg(long, global = true, default_value = tb_daemon::config::DEFAULT_CONFIG)]
    config: PathBuf,

    /// Use this database instead of the one the configuration names.
    #[arg(long, global = true)]
    database: Option<PathBuf>,

    /// Read rules from this directory instead of the configured one.
    #[arg(long, global = true)]
    policy_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// How a user is doing right now.
    Status {
        /// Which user; omit for everyone managed on this machine.
        user: Option<String>,
        /// Machine-readable output, one JSON object per user.
        #[arg(long)]
        json: bool,
    },
    /// Time spent, broken down by application.
    Usage {
        user: String,
        /// The whole policy week instead of today.
        #[arg(long)]
        week: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Show or change the rules for a user.
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// Grant extra time for today.
    GrantBonus {
        user: String,
        /// How much, for example `30m`.
        amount: String,
    },
    /// Add or remove the PAM module from the login stacks.
    #[command(subcommand)]
    Pam(PamCommand),
    /// Check whether this installation will actually enforce anything.
    Doctor {
        /// Inspect this directory instead of /etc/pam.d. Useful for checking an
        /// image or a chroot, and for the tool's own tests.
        #[arg(long, default_value = "/etc/pam.d")]
        pam_root: PathBuf,
        /// Socket to probe instead of the packaged one.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum PolicyCommand {
    /// Print the rules for a user.
    Show { user: String },
    /// Change the rules for a user.
    Set {
        user: String,
        #[command(flatten)]
        edit: EditArgs,
    },
    /// Print the file a user's rules live in.
    Path { user: String },
    /// Stop managing a user, deleting their rules. Usage is kept.
    Remove {
        user: String,
        /// Required, so this cannot happen by a slip of the shell.
        #[arg(long)]
        yes: bool,
    },
    /// Write a user's rules to a TOML file.
    Export {
        user: String,
        /// Where to write; omit for standard output.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Read a user's rules from a TOML file.
    Import {
        user: String,
        #[arg(long)]
        input: PathBuf,
    },
}

#[derive(Args, Debug)]
struct EditArgs {
    /// Turn enforcement on or off. Off records usage without limiting anything.
    #[arg(long)]
    enforcement: Option<bool>,

    /// IANA time zone, for example `Europe/Berlin`.
    #[arg(long)]
    timezone: Option<String>,

    /// When the policy day starts, for example `04:00`.
    #[arg(long, value_name = "HH:MM")]
    day_start: Option<String>,

    /// Daily quota. `2h` for every day, or `sat=3h` for one. Repeatable.
    #[arg(long, value_name = "[DAY=]DURATION")]
    daily: Vec<QuotaArg>,

    /// Weekly ceiling, or `unlimited`.
    #[arg(long, value_name = "DURATION")]
    weekly: Option<String>,

    /// Allowed hours, for example `mon=15:00-19:00`. Repeatable.
    #[arg(long, value_name = "DAY=HH:MM-HH:MM")]
    window: Vec<WindowArg>,

    /// Remove all allowed-hours restrictions before applying any --window.
    #[arg(long)]
    clear_windows: bool,

    /// Time to save work between locking and ending a session.
    #[arg(long, value_name = "DURATION")]
    grace: Option<DurationSpec>,

    /// How much inactivity stops the clock.
    #[arg(long, value_name = "DURATION")]
    idle: Option<DurationSpec>,

    /// What happens when time runs out.
    #[arg(long, value_parser = ["lock", "terminate", "lock_then_terminate"])]
    on_exhausted: Option<String>,

    /// Record window titles. Off by default, and worth leaving off.
    #[arg(long)]
    record_titles: Option<bool>,
}

impl EditArgs {
    fn into_edit(self) -> Result<PolicyEdit> {
        Ok(PolicyEdit {
            enforcement: self.enforcement,
            timezone: self.timezone,
            day_start: self
                .day_start
                .as_deref()
                .map(policyedit::parse_time)
                .transpose()?,
            daily: self.daily,
            weekly: self
                .weekly
                .as_deref()
                .map(|w| -> Result<Quota> {
                    Ok(if w.eq_ignore_ascii_case("unlimited") {
                        Quota::Unlimited
                    } else {
                        Quota::Limited(w.parse::<DurationSpec>()?)
                    })
                })
                .transpose()?,
            windows: self.window,
            clear_windows: self.clear_windows,
            grace_period: self.grace,
            idle_threshold: self.idle,
            on_exhausted: self.on_exhausted.as_deref().map(|a| match a {
                "terminate" => LockAction::Terminate,
                "lock_then_terminate" => LockAction::LockThenTerminate,
                _ => LockAction::Lock,
            }),
            record_window_titles: self.record_titles,
            warnings: None,
        })
    }
}

#[derive(Subcommand, Debug)]
enum PamCommand {
    /// Add the module to the login and lock-screen stacks.
    Enable {
        /// Show what would change without touching anything.
        #[arg(long)]
        dry_run: bool,
        /// Operate under this directory instead of /etc/pam.d.
        #[arg(long, default_value = "/etc/pam.d")]
        root: PathBuf,
        /// Also consider service files a distribution ships in this
        /// directory. Defaults to /usr/lib/pam.d, but only when --root is the
        /// real /etc/pam.d.
        #[arg(long)]
        vendor_root: Option<PathBuf>,
    },
    /// Remove the module again, restoring the stacks.
    Disable {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "/etc/pam.d")]
        root: PathBuf,
        /// Also consider service files a distribution ships in this
        /// directory. Defaults to /usr/lib/pam.d, but only when --root is the
        /// real /etc/pam.d.
        #[arg(long)]
        vendor_root: Option<PathBuf>,
    },
    /// Ask the daemon the question the module asks, and report the answer.
    ///
    /// Useful under `runcon`: the module runs inside the display manager, which
    /// on an `SELinux` system is confined differently from a shell, and a policy
    /// can permit the connect while denying the query.
    Probe {
        /// Socket to ask, instead of the packaged one.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Report which services carry the module.
    Status {
        #[arg(long, default_value = "/etc/pam.d")]
        root: PathBuf,
        /// Also consider service files a distribution ships in this
        /// directory. Defaults to /usr/lib/pam.d, but only when --root is the
        /// real /etc/pam.d.
        #[arg(long)]
        vendor_root: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tbctl: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    // PAM commands touch no database, so they still work when the daemon has
    // never run — which is exactly when someone needs `pam disable`.
    if let Command::Pam(cmd) = &cli.command {
        return pam_command(cmd);
    }

    let paths = resolve(&cli)?;
    let store = open_store(&paths)?;
    let now = Zoned::now();

    match cli.command {
        Command::Status { user, json } => status(&store, user.as_deref(), json, &now),
        Command::Usage { user, week, json } => usage(&store, &user, week, json, &now),
        Command::Policy(cmd) => policy_command(&store, cmd),
        Command::GrantBonus { user, amount } => grant_bonus(&store, &user, &amount, &now),
        Command::Doctor { pam_root, socket } => {
            doctor_command(&store, &paths, &pam_root, socket.as_deref())
        }
        Command::Pam(_) => unreachable!("handled above"),
    }
}

/// Where this invocation reads and writes, after the command line has had its
/// say over the configuration file.
struct Paths {
    database: PathBuf,
    policy_dir: PathBuf,
    managed_group: String,
}

fn resolve(cli: &Cli) -> Result<Paths> {
    let cfg =
        Config::load(&cli.config).with_context(|| format!("reading {}", cli.config.display()))?;
    Ok(Paths {
        database: cli.database.clone().unwrap_or_else(|| cfg.database_path()),
        policy_dir: cli.policy_dir.clone().unwrap_or(cfg.policy_dir),
        managed_group: cfg.managed_group.unwrap_or_else(|| "kids".to_owned()),
    })
}

fn open_store(paths: &Paths) -> Result<Store> {
    if !paths.database.exists() {
        bail!(
            "no database at {} — has timebanditsd ever run?",
            paths.database.display()
        );
    }
    Store::open(&paths.database, &paths.policy_dir)
        .with_context(|| format!("opening {}", paths.database.display()))
}

/// The same facts the human report is built from, in a shape a script can read.
///
/// Exists because scraping the prose is the kind of check that silently
/// succeeds against the wrong number. `null` means unlimited throughout —
/// never zero, which is the opposite.
fn status_json(
    policy: &Policy,
    snapshot: &tb_core::engine::UsageSnapshot,
    verdict: &tb_core::Verdict,
    day: tb_core::schedule::PolicyDay,
) -> serde_json::Value {
    let denial = verdict.denial();
    serde_json::json!({
        "subject": policy.subject,
        "policy_day": day.date.to_string(),
        "timezone": policy.timezone,
        "enforcement": policy.enforcement,
        "used_today_secs": snapshot.used_today.as_secs(),
        "used_week_secs": snapshot.used_this_week.as_secs(),
        "bonus_today_secs": snapshot.bonus_today.as_secs(),
        "allowed": verdict.is_allowed(),
        "remaining_secs": verdict.remaining().map(tb_core::DurationSpec::as_secs),
        "deny_reason": denial.map(|d| d.reason.message_key()),
        "retry_at": denial.and_then(|d| d.retry_at.as_ref()).map(ToString::to_string),
    })
}

fn subjects_or(store: &Store, user: Option<&str>) -> Result<Vec<String>> {
    match user {
        Some(u) => Ok(vec![u.to_owned()]),
        None => Ok(store.subjects()?),
    }
}

fn load_policy(store: &Store, user: &str) -> Result<Policy> {
    store
        .load_policy(user)?
        .with_context(|| format!("no policy for `{user}`"))
}

fn status(store: &Store, user: Option<&str>, json: bool, now: &Zoned) -> Result<ExitCode> {
    let subjects = subjects_or(store, user)?;
    if subjects.is_empty() {
        if !json {
            println!("no users are managed on this machine");
        }
        return Ok(ExitCode::SUCCESS);
    }
    for subject in subjects {
        let policy = load_policy(store, &subject)?;
        let (snapshot, day) = store.snapshot(&policy, now)?;
        let verdict = tb_core::evaluate(&policy, &snapshot, now);
        if json {
            println!(
                "{}",
                serde_json::to_string(&status_json(&policy, &snapshot, &verdict, day,))?
            );
        } else {
            print!("{}", report::status(&policy, &snapshot, &verdict, day, now));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn usage(store: &Store, user: &str, week: bool, json: bool, now: &Zoned) -> Result<ExitCode> {
    let policy = load_policy(store, user)?;
    let (_, day) = store.snapshot(&policy, now)?;

    let tz = jiff::tz::TimeZone::get(&policy.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let end = tb_core::schedule::policy_day_end(day, policy.day_start, &tz);
    let days_back = if week {
        i64::from(day.date.weekday().to_monday_zero_offset()) + 1
    } else {
        1
    };
    let start = end
        .checked_sub(jiff::Span::new().days(days_back))
        .unwrap_or_else(|_| end.clone());

    let segments = store.segments_between(user, start.timestamp(), now.timestamp())?;
    let totals = tb_core::usage::totals_by_app(&segments);
    let total = tb_core::usage::total(&segments);

    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "subject": user,
                "period": if week { "week" } else { "day" },
                "from": start.timestamp().to_string(),
                "total_secs": total.as_secs(),
                "apps": totals
                    .iter()
                    .map(|(app, d)| serde_json::json!({
                        "app": app.to_string(),
                        "secs": d.as_secs(),
                    }))
                    .collect::<Vec<_>>(),
            }))?
        );
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "{user}: {} to now",
        if week { "this policy week" } else { "today" }
    );
    print!("{}", report::usage_table(&totals, total));
    Ok(ExitCode::SUCCESS)
}

fn policy_command(store: &Store, cmd: PolicyCommand) -> Result<ExitCode> {
    match cmd {
        PolicyCommand::Show { user } => {
            print!("{}", report::policy(&load_policy(store, &user)?));
            // Rules are a file, so say which one. A parent who wants something
            // this summary cannot express can open it in an editor.
            if let Ok(path) = store.policies().path_for(&user) {
                println!("\nrules: {}", path.display());
            }
        }
        PolicyCommand::Path { user } => {
            let path = store.policies().path_for(&user)?;
            println!("{}", path.display());
            if !path.exists() {
                bail!("`{user}` is not managed on this machine — no such file");
            }
        }
        PolicyCommand::Remove { user, yes } => {
            if !yes {
                bail!("this deletes the rules for `{user}` — pass --yes if you mean it");
            }
            if store.delete_policy(&user)? {
                println!("`{user}` is no longer managed; recorded usage is kept");
            } else {
                println!("`{user}` was not managed here; nothing to do");
            }
        }
        PolicyCommand::Set { user, edit } => {
            let edit = edit.into_edit()?;
            if edit.is_empty() {
                bail!("nothing to change — see `tbctl policy set --help`");
            }
            // A user with no policy yet starts from a permissive one, so
            // `policy set` is also how a child is first taken under management.
            let base = store
                .load_policy(&user)?
                .unwrap_or_else(|| Policy::permissive(&user));
            let updated = edit.apply(&base)?;
            if !store.save_policy(&updated)? {
                bail!("a newer policy for `{user}` already exists; refusing to go backwards");
            }
            println!("updated `{user}` to version {}", updated.version);
            print!("{}", report::policy(&updated));
        }
        PolicyCommand::Export { user, output } => {
            let policy = load_policy(store, &user)?;
            let text = toml::to_string_pretty(&policy)?;
            match output {
                Some(path) => {
                    std::fs::write(&path, &text)
                        .with_context(|| format!("writing {}", path.display()))?;
                    println!("wrote {}", path.display());
                }
                None => print!("{text}"),
            }
        }
        PolicyCommand::Import { user, input } => {
            let text = std::fs::read_to_string(&input)
                .with_context(|| format!("reading {}", input.display()))?;
            let mut policy: Policy = toml::from_str(&text)?;
            if policy.subject != user {
                // Importing one child's rules onto another by accident is a
                // mistake worth refusing rather than silently rewriting.
                bail!(
                    "the file describes `{}`, not `{user}` — pass the matching user",
                    policy.subject
                );
            }
            policy.validate()?;
            let current = store.load_policy(&user)?.map_or(0, |p| p.version);
            policy.version = current.saturating_add(1);
            store.save_policy(&policy)?;
            println!("imported `{user}` as version {}", policy.version);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn grant_bonus(store: &Store, user: &str, amount: &str, now: &Zoned) -> Result<ExitCode> {
    let amount: DurationSpec = amount.parse()?;
    let policy = load_policy(store, user)?;
    let (_, day) = store.snapshot(&policy, now)?;

    let granted_by = std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown".to_owned());
    store.add_bonus(user, day, amount, &granted_by)?;

    println!(
        "granted {} to `{user}` for {}",
        report::human(amount),
        day.date
    );
    let (snapshot, day) = store.snapshot(&policy, now)?;
    let verdict = tb_core::evaluate(&policy, &snapshot, now);
    print!("{}", report::status(&policy, &snapshot, &verdict, day, now));
    Ok(ExitCode::SUCCESS)
}

/// A `PamDir` for the given root, with the vendor directory attached only when
/// that root is the real one.
///
/// A distribution can ship a service file in `/usr/lib/pam.d` and nowhere else
/// — Fedora 44 does exactly that with `plasmalogin` — so the vendor directory
/// has to be looked at. Pointing a throwaway root at the live vendor directory
/// would be nonsense, though, so `--root` turns it off unless the caller says
/// otherwise.
fn pam_dir(root: &std::path::Path, vendor: Option<&std::path::Path>) -> pamconf::PamDir {
    let dir = pamconf::PamDir::new(root);
    match vendor {
        Some(v) => dir.with_vendor(v),
        None if root == std::path::Path::new("/etc/pam.d") => dir.with_vendor(pamconf::VENDOR_DIR),
        None => dir,
    }
}

fn pam_command(cmd: &PamCommand) -> Result<ExitCode> {
    match cmd {
        PamCommand::Enable {
            dry_run,
            root,
            vendor_root,
        } => {
            let pam = pam_dir(root, vendor_root.as_deref());
            if *dry_run {
                println!("dry run — nothing will be written");
            }
            for change in pam.enable(*dry_run)? {
                println!("{change}");
            }
            if !*dry_run {
                println!(
                    "\nBackups of the originals are alongside each file, suffixed `{}`.",
                    pamconf::BACKUP_SUFFIX
                );
                println!("If anything goes wrong, log in as a parent or root — both are");
                println!("exempt before any of this takes effect — and run `tbctl pam disable`.");
            }
        }
        PamCommand::Disable {
            dry_run,
            root,
            vendor_root,
        } => {
            let pam = pam_dir(root, vendor_root.as_deref());
            for change in pam.disable(*dry_run)? {
                println!("{change}");
            }
        }
        PamCommand::Probe { socket } => {
            let path = socket
                .clone()
                .unwrap_or_else(|| PathBuf::from(tb_proto::pam::SOCKET_PATH));
            match doctor::probe(&path) {
                Ok(()) => println!("  ok       {} answered", path.display()),
                Err(e) => {
                    println!("  FAIL     {e}");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        PamCommand::Status { root, vendor_root } => {
            let pam = pam_dir(root, vendor_root.as_deref());
            for (spec, state) in pam.status()? {
                let mark = match state {
                    pamconf::ServiceState::Configured => "configured",
                    pamconf::ServiceState::NotConfigured => "NOT configured",
                    pamconf::ServiceState::Absent => "not installed",
                };
                // Naming the file saves the reader a guess about which of the
                // several plausible paths this system actually uses.
                println!(
                    "  {:<8} {:<15} {}\n           {}",
                    spec.service,
                    mark,
                    spec.why,
                    pam.service_path(spec.service).display()
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn doctor_command(
    store: &Store,
    paths: &Paths,
    pam_root: &std::path::Path,
    socket: Option<&std::path::Path>,
) -> Result<ExitCode> {
    // A policy that will not parse must show up as a failure, not vanish from
    // the report — a silent disappearance is how a child ends up unlimited
    // while the report says everything is fine.
    let mut unreadable = Vec::new();
    let mut policies: Vec<Policy> = Vec::new();
    for subject in store.subjects()? {
        match store.load_policy(&subject) {
            Ok(Some(p)) => policies.push(p),
            Ok(None) => {}
            Err(e) => unreadable.push(e.to_string()),
        }
    }

    let mut env = doctor::Environment {
        pam: pam_dir(pam_root, None),
        database: paths.database.clone(),
        disable_flag: PathBuf::from(tb_daemon::config::DISABLE_FLAG),
        managed_group: paths.managed_group.clone(),
        ..doctor::Environment::default()
    };
    if let Some(s) = socket {
        env.socket = s.to_path_buf();
    }
    let checks = doctor::run(&env, &policies);
    for check in &checks {
        println!("{check}");
    }
    for problem in &unreadable {
        println!("  [FAIL] {:<22} {problem}", "policy file");
    }

    let worst = if unreadable.is_empty() {
        doctor::worst(&checks)
    } else {
        doctor::Level::Fail
    };
    Ok(match worst {
        doctor::Level::Fail => {
            println!("\nEnforcement is not working. Fix the FAIL lines above.");
            ExitCode::FAILURE
        }
        doctor::Level::Warn => {
            println!("\nWorks, but check the warnings — they are the ways to be");
            println!("quietly ineffective.");
            ExitCode::SUCCESS
        }
        doctor::Level::Ok => {
            println!("\nEverything checks out.");
            ExitCode::SUCCESS
        }
    })
}
