//! The Applications this program serves.
//!
//! For now they are fixture scripts named at start-up
//! (`--application <name>=<path>`). The platform database replaces this
//! (Application registry, Versions), so it is kept thin: a name, and the
//! script that runs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// An Application as the Request runner needs it.
#[derive(Debug)]
pub struct Application {
    /// Its name, which is also its subdomain (ADR 0011).
    pub name: Arc<str>,
    /// One classic script that registers
    /// `globalThis.default = { fetch(request, ctx) }`.
    pub script: Arc<str>,
}

/// Every Application this program serves, by name.
#[derive(Debug, Default)]
pub struct Applications {
    by_name: HashMap<Arc<str>, Arc<Application>>,
}

impl Applications {
    /// Reads each fixture given as `<name>=<path>`. Refuses none at all, a
    /// spec without `=`, a name that is not a DNS label, a name given
    /// twice, or a file it cannot read.
    pub fn from_specs(specs: &[String]) -> Result<Self, String> {
        if specs.is_empty() {
            return Err(
                "no Application: name at least one with --application <name>=<path>".into(),
            );
        }
        let mut by_name = HashMap::new();
        for spec in specs {
            let Some((name, path)) = spec.split_once('=') else {
                return Err(format!(
                    "--application {spec:?} is not <name>=<path>: the Application has no name"
                ));
            };
            check_name(name)?;
            let path = Path::new(path);
            let script = std::fs::read_to_string(path).map_err(|e| {
                format!(
                    "cannot read the Application {name} from {}: {e}",
                    path.display()
                )
            })?;
            let name: Arc<str> = name.into();
            let application = Arc::new(Application {
                name: Arc::clone(&name),
                script: script.into(),
            });
            if by_name.insert(name, application).is_some() {
                return Err(format!(
                    "the Application name in --application {spec:?} is given twice"
                ));
            }
        }
        Ok(Self { by_name })
    }

    pub fn get(&self, name: &str) -> Option<&Arc<Application>> {
        self.by_name.get(name)
    }

    /// The names, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.by_name.keys().map(AsRef::as_ref).collect();
        names.sort_unstable();
        names
    }
}

/// An Application's name is its subdomain, so it must be a DNS label:
/// lowercase letters, digits and hyphens, 1 to 63 long, no hyphen at either
/// end.
pub fn check_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "the Application name {name:?} is not a DNS label: use 1 to 63 lowercase letters, digits and hyphens, not starting or ending with a hyphen"
        ))
    }
}
