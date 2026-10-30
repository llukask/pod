//! The window's asset source: gpui-kit's default icons, the ones Pod uses
//! that the default bundle leaves out, and Pod's own icon set
//! ([`crate::shell::icon`]).

use std::borrow::Cow;

use gpui_kit::SharedString;
use gpui_kit::assets::{Assets, IconNamed as _, icon_assets};

use crate::shell::icon::{PREFIX, PodIcon};

// gpui-kit embeds only the ~100 icons its own components need. Every other
// `IconName` compiles fine but fails at render time with "could not find
// asset", once per frame. Each icon used outside the default list has to be
// added here; the test below catches the ones that aren't.
icon_assets!(ExtraIcons, [Archive, ArrowUpDown, CloudOff]);

pub struct AppAssets;

impl gpui_kit::AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(icon) = PodIcon::from_path(path) {
            return Ok(Some(Cow::Owned(icon.svg().into_bytes())));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        if PREFIX.starts_with(path) {
            paths.extend(PodIcon::ALL.map(|icon| icon.path()));
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gpui_kit::AssetSource as _;

    use super::*;

    /// Scans the crate's source for `IconName` variants and loads each, since
    /// a missing one only shows up as log spam in the running app.
    #[test]
    fn every_icon_the_app_names_is_embedded() {
        let mut names = std::collections::BTreeSet::new();
        collect_icon_names(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
            &mut names,
        );
        assert!(!names.is_empty(), "the scan finds the app's icons");

        let missing: Vec<_> = names
            .iter()
            .map(|name| format!("icons/{}.svg", kebab_case(name)))
            .filter(|path| !matches!(AppAssets.load(path), Ok(Some(_))))
            .collect();
        assert!(missing.is_empty(), "add these to `ExtraIcons`: {missing:?}");
    }

    fn collect_icon_names(dir: &Path, names: &mut std::collections::BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("the source directory is readable") {
            let path = entry.expect("the directory entry is readable").path();
            if path.is_dir() {
                collect_icon_names(&path, names);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let source = std::fs::read_to_string(&path).expect("source files are UTF-8");
                for rest in source.split("IconName::").skip(1) {
                    let name: String = rest
                        .chars()
                        .take_while(char::is_ascii_alphanumeric)
                        .collect();
                    if !name.is_empty() {
                        names.insert(name);
                    }
                }
            }
        }
    }

    /// `ArrowUpDown` -> `arrow-up-down`, the Lucide file name. A digit starts
    /// a new segment too, as in `Volume2` -> `volume-2`.
    fn kebab_case(name: &str) -> String {
        let mut out = String::new();
        let mut previous_digit = false;
        for c in name.chars() {
            let boundary = c.is_ascii_uppercase() || (c.is_ascii_digit() && !previous_digit);
            if boundary && !out.is_empty() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            previous_digit = c.is_ascii_digit();
        }
        out
    }
}
