use tw_migrate_snapshots::{default_setup, run_case};

macro_rules! snapshot_cases {
    ($($case:ident => $setup:expr),+ $(,)?) => {
        $(
            #[test]
            fn $case() {
                let document = run_case(stringify!($case), $setup)
                    .unwrap_or_else(|error| panic!("{error}"));
                let mut settings = insta::Settings::clone_current();
                settings.set_snapshot_path(concat!(env!("CARGO_MANIFEST_DIR"), "/snapshots"));
                settings.bind(|| insta::assert_snapshot!(stringify!($case), document));
            }
        )+
    };
}

snapshot_cases! {
    cli_help => default_setup,
    cli_parser_failure => default_setup,
    module_flow => default_setup,
}

#[path = "cli_snapshots/styles.rs"]
mod styles;

#[path = "cli_snapshots/html_workspaces.rs"]
mod html_workspaces;

#[path = "cli_snapshots/safety.rs"]
mod safety;
