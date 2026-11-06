const WORKFLOWS: &[(&str, &str)] = &[
    ("code-review", include_str!("bundled/code-review.js")),
    ("deep-research", include_str!("bundled/deep-research.js")),
];

pub(crate) fn names() -> impl Iterator<Item = &'static str> {
    WORKFLOWS.iter().map(|(name, _)| *name)
}

pub(crate) fn get(name: &str) -> Option<&'static str> {
    WORKFLOWS
        .iter()
        .find_map(|(candidate, source)| (*candidate == name).then_some(*source))
}

#[cfg(test)]
#[path = "bundled_tests.rs"]
mod tests;
