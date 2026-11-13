//! "What's new" after an update: the plain-English `### Highlights` of `CHANGELOG.md`, built into the binary so
//! the text always matches the installed version. Shown once per update (not in the Host edition; see `docs/auto-update.md` at the repository root).

/// The changelog at build time.
pub const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");
/// Only this many newest changelog versions are considered.
const MAX_VERSIONS: usize = 5;

/// One version's highlights.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub items: Vec<String>,
}

fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.trim().split('.').map(|p| p.parse::<u32>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// The highlights of the newest [`MAX_VERSIONS`] versions in `changelog`, newest first. Versions without a
/// `### Highlights` section are left out: better nothing than technical text.
pub fn highlights(changelog: &str) -> Vec<Release> {
    let mut releases: Vec<Release> = Vec::new();
    let mut versions = 0;
    let mut in_highlights = false;
    for line in changelog.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            in_highlights = false;
            let Some(version) = rest.split(']').next().filter(|v| parse_version(v).is_some()) else { continue };
            versions += 1;
            if versions > MAX_VERSIONS {
                break;
            }
            releases.push(Release { version: version.to_string(), items: Vec::new() });
        } else if line.starts_with("### ") {
            in_highlights = line.trim_end() == "### Highlights";
        } else if in_highlights
            && let Some(item) = line.strip_prefix("- ")
            && let Some(r) = releases.last_mut()
        {
            r.items.push(item.trim().to_string());
        }
    }
    releases.retain(|r| !r.items.is_empty());
    releases
}

/// What to show when `current` starts: the releases after `last_seen` up to `current`, newest first. Nothing for
/// a new install (`last_seen` empty), a rollback or a restart on the same version, or when the user chose
/// "Don't show again" (`show` false).
pub fn to_show(releases: &[Release], last_seen: &str, current: &str, show: bool) -> Vec<Release> {
    let (Some(seen), Some(now)) = (parse_version(last_seen), parse_version(current)) else { return Vec::new() };
    if !show || seen >= now {
        return Vec::new();
    }
    releases.iter().filter(|r| parse_version(&r.version).is_some_and(|v| v > seen && v <= now)).cloned().collect()
}

/// The modal's body: bullets, with a version heading per release when several are shown.
pub fn body(releases: &[Release]) -> String {
    let several = releases.len() > 1;
    let mut out = Vec::new();
    for r in releases {
        if several {
            out.push(format!("{}{}", if out.is_empty() { "" } else { "\n" }, r.version));
        }
        out.extend(r.items.iter().map(|i| format!("•  {i}")));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "# Changelog\n\n## [Unreleased]\n\n## [1.3.1] - x\n\n### Highlights\n- Fix one.\n\n### Fixed\n- tech\n\n\
        ## [1.3.0] - x\n\n### Highlights\n- Search.\n- Tabs.\n\n### Added\n- `ENV_VAR`\n\n\
        ## [1.2.1] - x\n\n### Added\n- tech\n\n## [1.2.0] - x\n### Highlights\n- Old.\n";

    fn r(version: &str, items: &[&str]) -> Release {
        Release { version: version.into(), items: items.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn parses_highlights_newest_first_and_skips_versions_without_them() {
        assert_eq!(
            highlights(LOG),
            vec![r("1.3.1", &["Fix one."]), r("1.3.0", &["Search.", "Tabs."]), r("1.2.0", &["Old."])]
        );
        assert!(highlights("# Changelog\n").is_empty());
        // The changelog shipped with the build parses.
        let _ = highlights(CHANGELOG);
    }

    #[test]
    fn only_the_newest_versions_are_kept() {
        let log: String = (0..8).map(|i| format!("## [1.0.{}]\n### Highlights\n- v{}\n", 9 - i, 9 - i)).collect();
        let versions: Vec<_> = highlights(&log).into_iter().map(|r| r.version).collect();
        assert_eq!(versions, ["1.0.9", "1.0.8", "1.0.7", "1.0.6", "1.0.5"]);
    }

    #[test]
    fn shown_once_after_an_upgrade_only() {
        let all = highlights(LOG);
        assert_eq!(to_show(&all, "1.3.0", "1.3.1", true), vec![r("1.3.1", &["Fix one."])]);
        // Skipped versions: all of them in between, newest first; 1.2.1 has no highlights.
        let skipped: Vec<_> = to_show(&all, "1.1.0", "1.3.1", true).into_iter().map(|r| r.version).collect();
        assert_eq!(skipped, ["1.3.1", "1.3.0", "1.2.0"]);
        assert!(to_show(&all, "", "1.3.1", true).is_empty(), "new install");
        assert!(to_show(&all, "1.3.1", "1.3.0", true).is_empty(), "rollback");
        assert!(to_show(&all, "1.3.1", "1.3.1", true).is_empty(), "same version");
        assert!(to_show(&all, "1.3.0", "1.3.1", false).is_empty(), "don't show again");
        assert!(to_show(&all, "1.2.0", "1.2.1", true).is_empty(), "no highlights for 1.2.1");
        // A newer changelog entry than the running build is not shown.
        assert!(to_show(&all, "1.2.1", "1.3.0", true).iter().all(|r| r.version == "1.3.0"));
    }

    #[test]
    fn body_has_version_headings_only_for_several_releases() {
        assert_eq!(body(&[r("1.3.0", &["A.", "B."])]), "•  A.\n•  B.");
        assert_eq!(body(&[r("1.3.1", &["C."]), r("1.3.0", &["A."])]), "1.3.1\n•  C.\n\n1.3.0\n•  A.");
    }
}
