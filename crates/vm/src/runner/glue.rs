//! The script a request's context runs: the platform's glue
//! (`prelude.js`), the Application's script, then the line that hands the
//! Application's handler to the glue.

use std::sync::LazyLock;

/// `prelude.js` as one line, so that the Application's script keeps its
/// line numbers in stacks: comment lines dropped, the rest joined with
/// spaces (every statement in it ends with a semicolon).
fn prelude() -> String {
    include_str!("prelude.js")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run once the Application's script has run to its end; it never runs if
/// the script throws while loading.
const EPILOGUE: &str = "__vm_platform_glue_5c1e.install();";

/// The whole script for one context.
pub fn script(application: &str) -> String {
    static PRELUDE: LazyLock<String> = LazyLock::new(prelude);
    let prelude: &str = &PRELUDE;
    let mut script = String::with_capacity(prelude.len() + application.len() + EPILOGUE.len() + 4);
    script.push_str(prelude);
    script.push(' ');
    script.push_str(application);
    // A last line comment or a missing semicolon in the Application's
    // script must not swallow or join the epilogue.
    script.push_str("\n;");
    script.push_str(EPILOGUE);
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prelude_is_one_line() {
        assert!(!prelude().contains('\n'));
    }

    #[test]
    fn the_application_starts_on_the_first_line() {
        let script = script("a;\nb;");
        let mut lines = script.lines();
        assert!(lines.next().is_some_and(|l| l.ends_with(" a;")));
        assert_eq!(lines.next(), Some("b;"));
    }
}
