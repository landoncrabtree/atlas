//! Default values for every configuration struct.

use super::schema::{
    Density, DetailsView, General, Indexer, Navigation, Search, SortKey, SortOrder, Thumbnails,
    Ui, View, ViewMode,
};

impl Default for General {
    fn default() -> Self {
        Self {
            start_path: None,
            confirm_on_quit: false,
            follow_symlinks: true,
            vim_mode: false,
            dual_pane: true,
        }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: "atlas-dark".to_string(),
            font_family: "Inter".to_string(),
            font_size: 14.0,
            monospace_font_family: "JetBrains Mono".to_string(),
            density: Density::Comfortable,
            show_status_bar: true,
            show_breadcrumbs: true,
            show_shortcuts: true,
            animations: true,
            active_pane_border_px: 2.0,
        }
    }
}

impl Default for View {
    fn default() -> Self {
        Self {
            default_mode: ViewMode::Details,
            show_hidden: false,
            natural_sort: true,
            dirs_first: true,
            default_sort_key: SortKey::Name,
            default_sort_order: SortOrder::Asc,
            details: DetailsView::default(),
        }
    }
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            history_size: 100,
            remember_last_location: true,
            last_location: None,
        }
    }
}

impl Default for Indexer {
    fn default() -> Self {
        Self {
            enabled: true,
            roots: Vec::new(),
            respect_gitignore: true,
            max_memory_mb: 256,
        }
    }
}

impl Default for Search {
    fn default() -> Self {
        Self {
            fuzzy_max_results: 200,
            content_search_threads: None,
            default_globs_exclude: vec![
                ".git/".to_string(),
                "node_modules/".to_string(),
                "target/".to_string(),
            ],
        }
    }
}

impl Default for Thumbnails {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_max_size_mb: 500,
            generation_threads: None,
            generate_for_size_up_to_mb: 100,
        }
    }
}
