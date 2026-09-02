//! Finupdate — system update frontend for uupd.
//!
//! Entry point pattern for Bluefin utility apps:
//! 1. Initialize tracing (structured logging)
//! 2. Create an `adw::Application` via relm4 with a proper app ID
//! 3. Hand control to the relm4 component tree
//!
//! This pattern ensures:
//! - D-Bus activation works (app ID matches .desktop file)
//! - Single-instance behavior is enforced by GApplication
//! - libadwaita styles are loaded before any widgets are created

// The library is the single owner of the GUI module tree. Importing its public
// surface here avoids compiling app.rs and ui/** a second time into the binary
// with distinct crate-local type identities.
use finupdate::{action_journal, app::App, config, service, settings};

const USAGE: &str = "\
finupdate — system update frontend for bootc / uupd.

Usage: finupdate [OPTIONS]

Options:
  --dev-mode             Force developer mode for this run (simulated updates,
                         no destructive subprocesses).
  --no-dev-mode          Force developer mode OFF for this run. Use this if an
                         older build persisted dev_mode=true into settings.json
                         — while that is set, every update the GUI runs is
                         silently simulated.
  --sim=<scenario>       Pre-select a developer-mode simulation outcome:
                         success | failure | up-to-date. Implies --dev-mode.
  --dry-run              Block every destructive host command (reboot,
                         bootc switch, uupd timer/config writes) and report
                         synthetic success. Unlike --dev-mode the real update
                         worker still runs, so you exercise production code
                         paths without mutating the system.
  --no-dry-run           Force dry-run OFF. Development builds (including the
                         Devel Flatpak) default to dry-run, so use this to make
                         one perform real actions.
  --journal=<path>       Append a JSONL record of every intended privileged
                         action to <path>. Each line carries the exact argv a
                         real run would have executed, so tests can assert the
                         backend would do the right thing. Implies nothing on
                         its own — combine with --dry-run to assert safely.
  --help, -h             Print this message and exit.

All of these are **per-run only** — they are layered over settings.json in
memory and never written back, so running the app with a test flag can't leave
your saved configuration in developer mode. The hamburger menu doesn't expose
these toggles per HIG (no dev-only state visible to end users).
";

fn main() {
    // Initialize structured logging — respects RUST_LOG env var.
    // Default to "info" for release, "debug" for dev builds.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // CLI args — parse before settings::Settings::load() so the overrides
    // apply to the very first read. We don't depend on `clap` to keep the
    // binary small and to avoid an extra crate dep for two switches.
    let args: Vec<String> = std::env::args().collect();
    let mut force_dev_mode: Option<bool> = None;
    let mut force_dry_run: Option<bool> = None;
    let mut sim_scenario: Option<&str> = None;
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--dev-mode" => force_dev_mode = Some(true),
            // Needed because a settings.json written by an older build may
            // still carry dev_mode=true — that used to be persisted by
            // `--dev-mode`, which meant every update the GUI ran afterwards was
            // silently simulated. Without an explicit off-switch there is no
            // way to escape that state from the command line.
            "--no-dev-mode" => force_dev_mode = Some(false),
            "--dry-run" => force_dry_run = Some(true),
            // Symmetric escape hatch. Development builds (PROFILE=Devel, which
            // the Devel Flatpak sets) now default to dry_run=true, so without
            // this there is no way to make a Devel build actually do anything.
            "--no-dry-run" => force_dry_run = Some(false),
            "-h" | "--help" => {
                print!("{}", USAGE);
                std::process::exit(0);
            }
            a if a.starts_with("--journal=") => {
                // The journal sink is discovered through the environment so
                // that any helper process finupdate spawns (finupdate-cli, the
                // runner script) appends to the same file without needing the
                // path threaded through its own argv.
                let path = &a["--journal=".len()..];
                if path.is_empty() {
                    eprintln!("finupdate: --journal= requires a path");
                    std::process::exit(2);
                }
                // SAFETY: single-threaded here — this runs before the GTK main
                // loop and before any worker threads are spawned.
                unsafe {
                    std::env::set_var(action_journal::JOURNAL_ENV, path);
                }
            }
            a if a.starts_with("--sim=") => {
                let s = &a["--sim=".len()..];
                match s {
                    "success" | "failure" | "up-to-date" => {
                        sim_scenario = Some(match s {
                            "success" => "success",
                            "failure" => "failure",
                            _ => "up-to-date",
                        });
                        force_dev_mode = Some(true);
                    }
                    _ => {
                        eprintln!("finupdate: invalid --sim scenario '{}'", s);
                        eprintln!("Valid: success | failure | up-to-date");
                        std::process::exit(2);
                    }
                }
            }
            _ => {
                eprintln!("finupdate: unknown argument '{}'", arg);
                eprint!("\n{}", USAGE);
                std::process::exit(2);
            }
        }
    }

    // Layer the CLI overrides in memory. These deliberately do NOT call
    // Settings::save() — invoking the app with a test flag must not mutate the
    // user's stored configuration. Every later Settings::load() picks these up
    // automatically.
    let overrides = settings::RuntimeOverrides {
        dev_mode: force_dev_mode,
        dry_run: force_dry_run,
        sim_scenario: sim_scenario.map(|s| {
            match s {
                "success" => "Success",
                "failure" => "Failure",
                _ => "AlreadyUpToDate",
            }
            .to_string()
        }),
        mock_identity: None,
    };
    if let Some(v) = force_dev_mode {
        tracing::info!("CLI override: dev_mode = {v} (this run only)");
    }
    if let Some(v) = force_dry_run {
        tracing::info!("CLI override: dry_run = {v} (this run only)");
    }
    if let Some(s) = &overrides.sim_scenario {
        tracing::info!("CLI override: simulator scenario = {s} (this run only)");
    }
    settings::set_runtime_overrides(overrides);

    tracing::info!(
        "Starting Finupdate ({}) v{}",
        config::APP_ID,
        config::VERSION
    );

    // Install the process-wide UpdaterService before any UI builds — UI
    // components grab it via service::global() rather than threading an Arc
    // through every closure. Swap a mock here if integration-testing.
    service::init(service::BootcUpdaterService::new());

    // relm4::RelmApp handles:
    // - Creating the adw::Application (because we enabled the "libadwaita" feature)
    // - Calling adw::init() which loads Adwaita CSS and enables color scheme support
    // - Running the GLib main loop
    // Hand GTK an argv containing only argv[0].
    //
    // GApplication parses the command line itself and aborts with "Unknown
    // option --dry-run" on anything it doesn't recognise. Since every flag
    // above has already been consumed into RuntimeOverrides, there is nothing
    // left for GTK to interpret — passing the original argv through would just
    // make GTK reject our own flags.
    let app = relm4::RelmApp::new(config::APP_ID)
        .with_args(vec![args.first().cloned().unwrap_or_default()]);
    app.run::<App>(());
}
