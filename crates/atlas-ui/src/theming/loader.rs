//! [`ThemeLoader`] — resolves a theme ID to a [`ThemeTokens`] value.
//!
//! Resolution order:
//! 1. Built-in defaults (`atlas-dark`, `atlas-light`).
//! 2. User themes directory: `<dir>/<id>.toml`.
//!
//! The user themes directory defaults to the platform config dir
//! (`~/Library/Application Support/dev.atlas.atlas/themes/` on macOS,
//! `~/.config/atlas/themes/` on Linux, `%APPDATA%\Atlas\themes\` on Windows),
//! but can be overridden with the `ATLAS_THEMES_DIR` environment variable
//! (useful for tests).

use std::path::{Path, PathBuf};

use super::defaults;
use super::tokens::ThemeTokens;
use super::watcher::ThemeError;

/// Where a theme was loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSource {
    /// One of the built-in Atlas themes.
    BuiltIn,
    /// A user-supplied theme on disk.
    User,
}

/// Lightweight descriptor for listing available themes.
#[derive(Debug, Clone)]
pub struct ThemeDescriptor {
    /// Machine-readable theme ID (file stem).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Where this theme originates.
    pub source: ThemeSource,
}

/// Resolves theme IDs to [`ThemeTokens`].
pub struct ThemeLoader {
    user_themes_dir: PathBuf,
}

impl ThemeLoader {
    /// Construct a loader using the platform default user themes directory.
    ///
    /// Honors the `ATLAS_THEMES_DIR` environment variable as an override.
    pub fn new() -> Self {
        let dir = std::env::var("ATLAS_THEMES_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                directories::ProjectDirs::from("dev", "atlas", "atlas")
                    .map(|dirs| dirs.config_dir().join("themes"))
            })
            .unwrap_or_else(|| PathBuf::from("themes"));
        Self {
            user_themes_dir: dir,
        }
    }

    /// Construct a loader pointing at a specific user themes directory.
    ///
    /// The `ATLAS_THEMES_DIR` override is **not** consulted.
    pub fn with_user_dir(dir: PathBuf) -> Self {
        Self {
            user_themes_dir: dir,
        }
    }

    /// Load a theme by ID.
    ///
    /// 1. Checks built-in defaults first (`atlas-dark`, `atlas-light`).
    /// 2. Falls back to `<user_themes_dir>/<id>.toml`.
    ///
    /// # Errors
    ///
    /// - [`ThemeError::NotFound`] when no built-in or file matches.
    /// - [`ThemeError::Io`] on filesystem errors.
    /// - [`ThemeError::Parse`] on invalid TOML.
    pub fn load(&self, id: &str) -> Result<ThemeTokens, ThemeError> {
        match id {
            "atlas-dark" => return Ok(defaults::default_dark()),
            "atlas-light" => return Ok(defaults::default_light()),
            _ => {}
        }

        let path = self.user_themes_dir.join(format!("{id}.toml"));
        if path.exists() {
            let content = std::fs::read_to_string(&path).map_err(|source| ThemeError::Io {
                path: path.clone(),
                source,
            })?;
            let tokens = toml::from_str(&content).map_err(|error| ThemeError::Parse {
                id: id.to_owned(),
                message: error.to_string(),
            })?;
            return Ok(tokens);
        }

        Err(ThemeError::NotFound(id.to_owned()))
    }

    /// List all available themes.
    ///
    /// Built-in themes appear first. User themes appear after, and will
    /// replace a built-in entry if they share the same ID (user wins).
    pub fn list(&self) -> Vec<ThemeDescriptor> {
        let mut out = vec![
            ThemeDescriptor {
                id: "atlas-dark".to_owned(),
                name: "Atlas Dark".to_owned(),
                source: ThemeSource::BuiltIn,
            },
            ThemeDescriptor {
                id: "atlas-light".to_owned(),
                name: "Atlas Light".to_owned(),
                source: ThemeSource::BuiltIn,
            },
        ];

        if let Ok(entries) = std::fs::read_dir(&self.user_themes_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "toml")
                {
                    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                        continue;
                    };
                    let id = stem.to_owned();
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(tokens) = toml::from_str::<ThemeTokens>(&content) {
                            out.retain(|descriptor| descriptor.id != id);
                            out.push(ThemeDescriptor {
                                id,
                                name: tokens.name,
                                source: ThemeSource::User,
                            });
                        }
                    }
                }
            }
        }

        out
    }

    /// Ensure the user themes directory exists.
    ///
    /// On first creation, copies the built-in themes as editable seeds.
    /// Returns the (possibly newly created) directory path.
    ///
    /// # Errors
    ///
    /// [`ThemeError::Io`] on filesystem errors.
    pub fn ensure_user_dir(&self) -> Result<PathBuf, ThemeError> {
        if !self.user_themes_dir.exists() {
            std::fs::create_dir_all(&self.user_themes_dir).map_err(|source| ThemeError::Io {
                path: self.user_themes_dir.clone(),
                source,
            })?;
            for tokens in defaults::defaults() {
                let path = self.user_themes_dir.join(format!("{}.toml", tokens.id));
                let content = toml::to_string_pretty(&tokens)
                    .map_err(|error| ThemeError::Serialize(error.to_string()))?;
                std::fs::write(&path, content).map_err(|source| ThemeError::Io { path, source })?;
            }
        }
        Ok(self.user_themes_dir.clone())
    }

    /// Returns a reference to the user themes directory path.
    pub(crate) fn user_themes_dir_ref(&self) -> &Path {
        &self.user_themes_dir
    }
}

impl Default for ThemeLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn load_builtin_dark() {
        let loader = ThemeLoader::with_user_dir(PathBuf::from("/nonexistent"));
        let theme = loader
            .load("atlas-dark")
            .expect("built-in theme should load");
        assert_eq!(theme.id, "atlas-dark");
    }

    #[test]
    fn load_builtin_light() {
        let loader = ThemeLoader::with_user_dir(PathBuf::from("/nonexistent"));
        let theme = loader
            .load("atlas-light")
            .expect("built-in theme should load");
        assert_eq!(theme.id, "atlas-light");
    }

    #[test]
    fn load_not_found() {
        let loader = ThemeLoader::with_user_dir(PathBuf::from("/nonexistent"));
        assert!(matches!(
            loader.load("does-not-exist"),
            Err(ThemeError::NotFound(_))
        ));
    }

    #[test]
    fn load_user_theme() {
        let dir = TempDir::new().expect("tempdir");
        let mut theme = defaults::default_dark();
        theme.id = "my-theme".to_owned();
        theme.name = "My Theme".to_owned();
        let content = toml::to_string_pretty(&theme).expect("serialize theme");
        std::fs::write(dir.path().join("my-theme.toml"), content).expect("write theme");

        let loader = ThemeLoader::with_user_dir(dir.path().to_owned());
        let loaded = loader.load("my-theme").expect("user theme should load");
        assert_eq!(loaded.id, "my-theme");
        assert_eq!(loaded.name, "My Theme");
    }

    #[test]
    #[serial]
    fn load_user_theme_via_env() {
        let dir = TempDir::new().expect("tempdir");
        let mut theme = defaults::default_dark();
        theme.id = "env-theme".to_owned();
        theme.name = "Env Theme".to_owned();
        let content = toml::to_string_pretty(&theme).expect("serialize theme");
        std::fs::write(dir.path().join("env-theme.toml"), content).expect("write theme");

        std::env::set_var("ATLAS_THEMES_DIR", dir.path());
        let loader = ThemeLoader::new();
        let loaded = loader.load("env-theme").expect("env theme should load");
        std::env::remove_var("ATLAS_THEMES_DIR");

        assert_eq!(loaded.id, "env-theme");
    }

    #[test]
    fn list_includes_builtins() {
        let loader = ThemeLoader::with_user_dir(PathBuf::from("/nonexistent"));
        let list = loader.list();
        let ids: Vec<&str> = list
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect();
        assert!(ids.contains(&"atlas-dark"));
        assert!(ids.contains(&"atlas-light"));
    }

    #[test]
    fn list_user_theme_added() {
        let dir = TempDir::new().expect("tempdir");
        let mut theme = defaults::default_dark();
        theme.id = "extra-theme".to_owned();
        theme.name = "Extra Theme".to_owned();
        let content = toml::to_string_pretty(&theme).expect("serialize theme");
        std::fs::write(dir.path().join("extra-theme.toml"), content).expect("write theme");

        let loader = ThemeLoader::with_user_dir(dir.path().to_owned());
        let list = loader.list();
        let ids: Vec<&str> = list
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect();
        assert!(ids.contains(&"atlas-dark"));
        assert!(ids.contains(&"extra-theme"));
    }

    #[test]
    fn list_user_wins_on_collision() {
        let dir = TempDir::new().expect("tempdir");
        let mut theme = defaults::default_dark();
        theme.name = "Custom Dark".to_owned();
        let content = toml::to_string_pretty(&theme).expect("serialize theme");
        std::fs::write(dir.path().join("atlas-dark.toml"), content).expect("write theme");

        let loader = ThemeLoader::with_user_dir(dir.path().to_owned());
        let list = loader.list();
        let darks: Vec<&ThemeDescriptor> = list
            .iter()
            .filter(|descriptor| descriptor.id == "atlas-dark")
            .collect();
        assert_eq!(darks.len(), 1, "no duplicate ids");
        assert_eq!(darks[0].source, ThemeSource::User, "user should win");
    }

    #[test]
    fn ensure_user_dir_seeds_builtins() {
        let dir = TempDir::new().expect("tempdir");
        let themes_dir = dir.path().join("themes");
        let loader = ThemeLoader::with_user_dir(themes_dir.clone());

        let created = loader
            .ensure_user_dir()
            .expect("themes dir should be created");

        assert_eq!(created, themes_dir);
        assert!(themes_dir.join("atlas-dark.toml").exists());
        assert!(themes_dir.join("atlas-light.toml").exists());
    }
}
