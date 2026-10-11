//! Finding plugins on disk.

use std::path::{Path, PathBuf};

use crate::primitives::{AuraError, Result};

use crate::primitives::plugins::{PluginDescriptor, PluginFormat};

/// Result of scanning one or more directories.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Every plugin found, in discovery order.
    pub plugins: Vec<PluginDescriptor>,
    /// Bundles that could not be loaded, with the reason.
    pub failures: Vec<(PathBuf, String)>,
}

/// Where plugins of `format` are conventionally installed on this platform,
/// plus the entries of the `CLAP_PATH` / `VST3_PATH` environment variable
/// (colon-separated on Unix, semicolon-separated on Windows). Paths are
/// returned whether or not they exist.
pub fn default_search_paths(format: PluginFormat) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let env_var = match format {
        PluginFormat::Clap => "CLAP_PATH",
        PluginFormat::Vst3 => "VST3_PATH",
    };
    if let Some(list) = std::env::var_os(env_var) {
        paths.extend(std::env::split_paths(&list).filter(|p| !p.as_os_str().is_empty()));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match format {
        PluginFormat::Clap => {
            if cfg!(target_os = "macos") {
                if let Some(h) = &home {
                    paths.push(h.join("Library/Audio/Plug-Ins/CLAP"));
                }
                paths.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
            } else if cfg!(target_os = "windows") {
                if let Some(p) = std::env::var_os("COMMONPROGRAMFILES") {
                    paths.push(PathBuf::from(p).join("CLAP"));
                }
                if let Some(p) = std::env::var_os("LOCALAPPDATA") {
                    paths.push(PathBuf::from(p).join("Programs/Common/CLAP"));
                }
            } else {
                if let Some(h) = &home {
                    paths.push(h.join(".clap"));
                }
                paths.push(PathBuf::from("/usr/lib/clap"));
                paths.push(PathBuf::from("/usr/local/lib/clap"));
            }
        }
        PluginFormat::Vst3 => {
            if cfg!(target_os = "macos") {
                if let Some(h) = &home {
                    paths.push(h.join("Library/Audio/Plug-Ins/VST3"));
                }
                paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
            } else if cfg!(target_os = "windows") {
                if let Some(p) = std::env::var_os("COMMONPROGRAMFILES") {
                    paths.push(PathBuf::from(p).join("VST3"));
                }
            } else {
                if let Some(h) = &home {
                    paths.push(h.join(".vst3"));
                }
                paths.push(PathBuf::from("/usr/lib/vst3"));
                paths.push(PathBuf::from("/usr/local/lib/vst3"));
            }
        }
    }
    paths.dedup();
    paths
}

/// Scans `dir` recursively for bundles of `format` and loads each one to
/// read its descriptors. Bundles that fail to load are reported, not fatal.
///
/// Fails only if the format cannot be hosted at all (VST3, or CLAP with the
/// `clap` feature disabled) or `dir` cannot be read.
pub fn scan_directory(dir: impl AsRef<Path>, format: PluginFormat) -> Result<ScanReport> {
    let dir = dir.as_ref();
    if format == PluginFormat::Vst3 {
        return Err(crate::primitives::plugins::vst3::unsupported());
    }
    if !cfg!(feature = "clap") {
        return Err(AuraError::Message(
            "CLAP hosting is disabled (feature `clap`)".into(),
        ));
    }
    let mut report = ScanReport::default();
    let mut bundles = Vec::new();
    collect_bundles(dir, format.extension(), &mut bundles)
        .map_err(|e| AuraError::Message(format!("scan {}: {e}", dir.display())))?;
    bundles.sort();
    for bundle in bundles {
        load_descriptors(&bundle, format, &mut report);
    }
    Ok(report)
}

/// Scans every existing directory from [`default_search_paths`].
pub fn scan_default(format: PluginFormat) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    for dir in default_search_paths(format) {
        if dir.is_dir() {
            let part = scan_directory(&dir, format)?;
            report.plugins.extend(part.plugins);
            report.failures.extend(part.failures);
        }
    }
    Ok(report)
}

fn collect_bundles(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        {
            // A `.clap` is a shared library on Linux/Windows and a directory
            // bundle on macOS; either way we stop descending here.
            out.push(path);
        } else if path.is_dir() {
            collect_bundles(&path, ext, out)?;
        }
    }
    Ok(())
}

#[cfg(feature = "clap")]
fn load_descriptors(bundle: &Path, _format: PluginFormat, report: &mut ScanReport) {
    match crate::primitives::plugins::clap::ClapBundle::load(bundle) {
        Ok(b) => report.plugins.extend(b.descriptors()),
        Err(e) => report.failures.push((bundle.to_path_buf(), e.to_string())),
    }
}

#[cfg(not(feature = "clap"))]
fn load_descriptors(bundle: &Path, _format: PluginFormat, report: &mut ScanReport) {
    report.failures.push((
        bundle.to_path_buf(),
        "CLAP hosting is disabled (feature `clap`)".into(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_include_env_and_platform_entries() {
        std::env::set_var("CLAP_PATH", "/tmp/aura-clap-a:/tmp/aura-clap-b");
        let paths = default_search_paths(PluginFormat::Clap);
        assert!(paths.contains(&PathBuf::from("/tmp/aura-clap-a")));
        assert!(paths.contains(&PathBuf::from("/tmp/aura-clap-b")));
        assert!(
            paths.len() >= 3,
            "platform defaults follow the env entries: {paths:?}"
        );
        assert!(paths.iter().all(|p| !p.as_os_str().is_empty()));
        let vst3 = default_search_paths(PluginFormat::Vst3);
        assert!(!vst3.is_empty());
        assert!(vst3
            .iter()
            .any(|p| p.to_string_lossy().to_ascii_lowercase().contains("vst3")));
    }

    #[test]
    fn vst3_scanning_is_unsupported() {
        let dir = std::env::temp_dir();
        assert_eq!(
            scan_directory(&dir, PluginFormat::Vst3).unwrap_err(),
            crate::primitives::plugins::vst3::unsupported()
        );
    }

    #[cfg(feature = "clap")]
    #[test]
    fn broken_bundles_are_reported_not_fatal() {
        let dir = std::env::temp_dir().join(format!("aura-plugins-scan-{}", std::process::id()));
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("not-a-plugin.clap"),
            b"definitely not an ELF file",
        )
        .unwrap();
        std::fs::write(dir.join("ignored.txt"), b"").unwrap();
        let report = scan_directory(&dir, PluginFormat::Clap).unwrap();
        assert!(report.plugins.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].0.ends_with("not-a-plugin.clap"));
        assert!(!report.failures[0].1.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            scan_directory(&dir, PluginFormat::Clap).is_err(),
            "unreadable directory"
        );
        // scan_default only visits directories that exist; never fails on CLAP.
        std::env::set_var("CLAP_PATH", dir.to_string_lossy().to_string());
        assert!(scan_default(PluginFormat::Clap).is_ok());
    }
}
