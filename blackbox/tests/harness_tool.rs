//! The harness's own checks of what the tests of the tool stand on
//! (`blackbox/src/tool.rs`): a machine of the test's own whose environment
//! holds nothing of the test's, a search of the platform database that
//! finds what is there, and a listener that is not an installation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::PathBuf;

use blackbox::tool::{ConfigHome, Machine, NotAnInstallation, platform_tables_holding};
use blackbox::{Request, User, Vm, VmOptions};

#[test]
fn a_machine_runs_the_program_in_a_directory_of_its_own_and_is_removed_after() {
    let root: PathBuf;
    {
        let machine = Machine::with_config_home(ConfigHome::Unset).umask("000");
        root = machine.root().to_path_buf();
        assert!(machine.home().starts_with(&root));
        assert!(
            machine.home().starts_with(std::env::temp_dir()),
            "the machine's home must be under the temporary directory, never the real home"
        );
        let help = machine.run(&["--help"]);
        help.expect_code(0);
        assert!(machine.all_files().is_empty());
    }
    assert!(!root.exists(), "expected the machine removed when dropped");
}

#[test]
fn the_search_of_the_platform_database_finds_what_is_there() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let marker = "findme-0123456789abcdef";
    vm.api_as(&User::new("alice"))
        .create_application(marker)
        .expect_status(201);
    let tables = platform_tables_holding(vm.platform(), marker);
    assert!(
        !tables.is_empty(),
        "expected the search to find an Application's name in the platform database"
    );
    assert!(platform_tables_holding(vm.platform(), "not-there-fedcba9876543210").is_empty());
}

#[test]
fn not_an_installation_answers_something_no_installation_would() {
    let other = NotAnInstallation::start();
    let answer = Request::new(other.address(), "/v1/me", "api.vm.test").send();
    assert_eq!(answer.status, 200);
    assert!(serde_json::from_slice::<serde_json::Value>(answer.body()).is_err());
}
