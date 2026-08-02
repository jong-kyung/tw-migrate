use tw_migrate_snapshots::CaseContext;

fn default_setup(_: &CaseContext<'_>) -> Result<(), String> {
    Ok(())
}

// A macro rather than a fn so the assertion expands in the invoking module;
// insta bakes `module_path!` into snapshot file names.
macro_rules! assert_case_with {
    ($case:expr, $setup:expr, $verify:expr) => {{
        let case = $case;
        let document = tw_migrate_snapshots::run_case_with(case, $setup, $verify)
            .unwrap_or_else(|error| panic!("{error}"));
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path(concat!(env!("CARGO_MANIFEST_DIR"), "/snapshots"));
        settings.bind(|| insta::assert_snapshot!(case, document));
    }};
}

macro_rules! snapshot_cases {
    ($($case:ident => $setup:expr),+ $(,)?) => {
        $(
            #[test]
            fn $case() {
                assert_case_with!(stringify!($case), $setup, |_| Ok(()));
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
