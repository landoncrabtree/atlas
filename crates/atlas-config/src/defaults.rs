//! Default values for every configuration struct.

use super::schema::{
    Density, DetailsView, General, Indexer, Navigation, Remote, RemotePool, RemotePreview, Search,
    SortKey, SortOrder, Thumbnails, Ui, View, ViewMode,
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
            min_query_length: 2,
            max_visible_results: 100,
            debounce_ms: 150,
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

impl Default for RemotePool {
    fn default() -> Self {
        Self {
            idle_ttl_ms: 300_000,
            max_connections: 8,
        }
    }
}

impl Default for Remote {
    fn default() -> Self {
        Self {
            pool: RemotePool::default(),
            timeout_ms: std::collections::HashMap::new(),
            default_timeout_ms: 15_000,
            retries: std::collections::HashMap::new(),
            default_retries: 3,
            backoff_initial_ms: 100,
            backoff_max_ms: 5_000,
            backoff_multiplier: 2.0,
            preview: RemotePreview::default(),
        }
    }
}

impl Default for RemotePreview {
    fn default() -> Self {
        Self {
            cache_dir: None,
            max_bytes: 200_000_000,
            max_age_secs: 86_400,
            max_open_bytes: 100_000_000,
        }
    }
}
