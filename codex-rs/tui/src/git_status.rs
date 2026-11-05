//! Bounded workspace Git status collection for the composer header.

use std::path::Path;
use std::time::Duration;

use crate::workspace_command::WorkspaceCommand;
use crate::workspace_command::WorkspaceCommandExecutor;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GitStatusSummary {
    pub(crate) branch: String,
    pub(crate) changed: usize,
    pub(crate) untracked: usize,
    pub(crate) ahead: usize,
    pub(crate) behind: usize,
}

pub(crate) async fn collect_git_status_summary(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
) -> Option<GitStatusSummary> {
    let command = WorkspaceCommand::new(
        [
            "git",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain=2",
            "--branch",
            "-z",
            "--untracked-files=normal",
        ],
        cwd,
    )
    .timeout(Duration::from_secs(2));
    let output = runner.run(command).await.ok()?;
    output.success().then(|| parse_status(&output.stdout))
}

fn parse_status(output: &str) -> GitStatusSummary {
    let mut summary = GitStatusSummary::default();
    let mut entries = output.split('\0');
    let mut oid = None;
    while let Some(entry) = entries.next() {
        if let Some(branch) = entry.strip_prefix("# branch.head ") {
            summary.branch = branch.to_string();
        } else if let Some(value) = entry.strip_prefix("# branch.oid ") {
            oid = Some(value);
        } else if let Some(counts) = entry.strip_prefix("# branch.ab ") {
            if let Some((ahead, behind)) = counts.split_once(' ') {
                summary.ahead = ahead.trim_start_matches('+').parse().unwrap_or(0);
                summary.behind = behind.trim_start_matches('-').parse().unwrap_or(0);
            }
        } else {
            match entry.as_bytes().first() {
                Some(b'?') => summary.untracked += 1,
                Some(b'1' | b'u') => summary.changed += 1,
                Some(b'2') => {
                    summary.changed += 1;
                    // Renames have a second NUL-delimited path, not a status record.
                    entries.next();
                }
                _ => {}
            }
        }
    }
    if summary.branch == "(detached)" {
        summary.branch = format!(
            "detached@{}",
            oid.unwrap_or("unknown").chars().take(7).collect::<String>()
        );
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codext_status_handles_renames_and_detached_heads() {
        let summary = parse_status(
            "# branch.oid abcdef123\0# branch.head (detached)\0# branch.ab +2 -3\02 R. rename\0? old-name\01 .M changed\0? untracked\0u conflict\0",
        );
        assert_eq!(summary.branch, "detached@abcdef1");
        assert_eq!((summary.ahead, summary.behind), (2, 3));
        assert_eq!((summary.changed, summary.untracked), (3, 1));
    }
}
