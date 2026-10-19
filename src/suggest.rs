//! `SuggestedPackagesReporter`.

use komposer_package::io::Io;
use komposer_package::{Package, PackageRef};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub source: String,
    pub target: String,
    pub reason: String,
}

#[derive(Default)]
pub struct Suggestions {
    pub list: Vec<Suggestion>,
}

impl Suggestions {
    /// `addSuggestionsFromPackage()`.
    pub fn add_from_package(&mut self, p: &Package) {
        for (target, reason) in &p.suggests {
            let reason = match reason {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            self.list.push(Suggestion { source: p.pretty_name.clone(), target: target.clone(), reason });
        }
    }

    /// `getFilteredSuggestions()`: drops suggestions another installed package fulfills.
    pub fn filtered(&self, installed: &[PackageRef], only_dependents_of: Option<&Package>) -> Vec<Suggestion> {
        let source_filter: Option<Vec<String>> = only_dependents_of.map(|root| {
            let mut names: Vec<String> = root.requires.iter().chain(&root.dev_requires).map(|l| l.target.clone()).collect();
            names.push(root.name.clone());
            names
        });
        self.list
            .iter()
            .filter(|s| {
                // only another package providing the target fulfills it, not the one suggesting it
                let provided_by_others = installed.iter().any(|p| p.names(true).contains(&s.target) && p.name != s.source.to_lowercase());
                !provided_by_others && source_filter.as_ref().is_none_or(|f| f.contains(&s.source))
            })
            .cloned()
            .collect()
    }

    /// `outputMinimalistic()`.
    pub fn output_minimalistic(&self, io: &Io, installed: &[PackageRef]) {
        let n = self.filtered(installed, None).len();
        if n > 0 {
            io.write_error(&format!("<info>{n} package suggestions were added by new dependencies, use `composer suggest` to see details.</info>"));
        }
    }
}
