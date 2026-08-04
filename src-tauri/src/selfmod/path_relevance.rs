// Classify a changed repo-relative path into the cheapest reload it needs.
// Ported from electron/main/self-mod/path-relevance.ts.
//
//   Hmr             — renderer source; Vite hot-swaps it, state preserved
//   FullReload      — route tree / html / styles entry; reload the window
//   ProcessRestart  — main/preload/Tauri config; restart the app
//
// The agent edits mostly land in `Hmr`. The escalation tiers exist so a deeper
// edit doesn't silently fail to take effect.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadKind {
    Hmr,
    FullReload,
    ProcessRestart,
}

const PROCESS_RESTART_PREFIXES: &[&str] = &["electron/main/", "electron/preload/", "src-tauri/"];
const PROCESS_RESTART_FILES: &[&str] =
    &["electron.vite.config.ts", "package.json", "tsconfig.json"];

const FULL_RELOAD_EXACT: &[&str] = &["index.html", "src/routeTree.gen.ts"];
const FULL_RELOAD_PREFIXES: &[&str] = &["src/routes/"];

pub fn classify_path(repo_rel_path: &str) -> ReloadKind {
    let p = repo_rel_path.replace('\\', "/");

    if PROCESS_RESTART_FILES.contains(&p.as_str()) {
        return ReloadKind::ProcessRestart;
    }
    if PROCESS_RESTART_PREFIXES
        .iter()
        .any(|pre| p.starts_with(pre))
    {
        return ReloadKind::ProcessRestart;
    }

    if FULL_RELOAD_EXACT.contains(&p.as_str()) {
        return ReloadKind::FullReload;
    }
    if FULL_RELOAD_PREFIXES.iter().any(|pre| p.starts_with(pre)) {
        return ReloadKind::FullReload;
    }

    // Everything else under the renderer hot-swaps; unknown paths (docs,
    // scripts, micro-apps) also default here — no shell reload needed.
    ReloadKind::Hmr
}

/// True for paths the Vite dev server serves to the renderer and the self-mod
/// overlay (W1) can therefore pin to a baseline and swap atomically. Narrower
/// than the reload classifier: the overlay can't make main-process code,
/// configs, or package installs visible — those go through the reload driver's
/// restart tiers.
pub fn is_vite_trackable_path(repo_rel_path: &str) -> bool {
    let p = repo_rel_path.replace('\\', "/");
    p.starts_with("src/") || p == "index.html"
}

/// The strongest reload required by a batch of edits.
pub fn classify_batch(paths: &[String]) -> ReloadKind {
    let mut strongest = ReloadKind::Hmr;
    for path in paths {
        let kind = classify_path(path);
        if kind == ReloadKind::ProcessRestart {
            return ReloadKind::ProcessRestart;
        }
        if kind == ReloadKind::FullReload {
            strongest = ReloadKind::FullReload;
        }
    }
    strongest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_path_cases() {
        let cases: &[(&str, ReloadKind)] = &[
            ("src/app/chat/ChatApp.tsx", ReloadKind::Hmr),
            ("src/shell/Sidebar.tsx", ReloadKind::Hmr),
            ("src/styles/index.css", ReloadKind::Hmr),
            ("src/routes/chat.tsx", ReloadKind::FullReload),
            ("src/routes/__root.tsx", ReloadKind::FullReload),
            ("src/routeTree.gen.ts", ReloadKind::FullReload),
            ("index.html", ReloadKind::FullReload),
            ("electron/main/index.ts", ReloadKind::ProcessRestart),
            ("electron/main/agents/claude.ts", ReloadKind::ProcessRestart),
            ("electron/preload/index.ts", ReloadKind::ProcessRestart),
            ("electron.vite.config.ts", ReloadKind::ProcessRestart),
            ("package.json", ReloadKind::ProcessRestart),
            ("tsconfig.json", ReloadKind::ProcessRestart),
            ("docs/ARCHITECTURE.md", ReloadKind::Hmr),
            ("scripts/create-micro-app.mjs", ReloadKind::Hmr),
            ("micro-apps/demo/src/App.tsx", ReloadKind::Hmr),
            ("src\\routes\\chat.tsx", ReloadKind::FullReload),
            ("electron\\main\\index.ts", ReloadKind::ProcessRestart),
            ("src-tauri/src/main.rs", ReloadKind::ProcessRestart),
        ];
        for (input, expected) in cases {
            assert_eq!(classify_path(input), *expected, "{input}");
        }
    }

    #[test]
    fn prefix_vs_file_edge_cases() {
        // 'src/routes' (no trailing slash) is not a route file; falls through
        // to the generic src/ -> hmr bucket. Only 'src/routes/...' escalates.
        assert_eq!(classify_path("src/routes"), ReloadKind::Hmr);
        assert_eq!(classify_path("src/routes/x.tsx"), ReloadKind::FullReload);
    }

    #[test]
    fn preload_is_not_hmr() {
        assert_ne!(classify_path("electron/preload/index.ts"), ReloadKind::Hmr);
        assert_eq!(
            classify_path("electron/preload/index.ts"),
            ReloadKind::ProcessRestart
        );
    }

    #[test]
    fn classify_batch_strongest_wins() {
        assert_eq!(
            classify_batch(&[
                "src/app/chat/ChatApp.tsx".into(),
                "src/shell/Sidebar.tsx".into()
            ]),
            ReloadKind::Hmr
        );
        assert_eq!(
            classify_batch(&["src/shell/Sidebar.tsx".into(), "src/routes/chat.tsx".into()]),
            ReloadKind::FullReload
        );
        assert_eq!(
            classify_batch(&[
                "src/shell/Sidebar.tsx".into(),
                "src/routes/chat.tsx".into(),
                "electron/main/index.ts".into(),
            ]),
            ReloadKind::ProcessRestart
        );
        assert_eq!(
            classify_batch(&[
                "electron/main/index.ts".into(),
                "src/routes/chat.tsx".into()
            ]),
            ReloadKind::ProcessRestart
        );
        assert_eq!(classify_batch(&[]), ReloadKind::Hmr);
    }

    #[test]
    fn is_vite_trackable_path_cases() {
        let yes = [
            "src/app/chat/ChatApp.tsx",
            "src/shell/Rail.tsx",
            "src/styles/hearth.css",
            "index.html",
        ];
        let no = [
            "electron/main/index.ts",
            "electron/preload/index.ts",
            "package.json",
            "docs/x.md",
        ];
        for p in yes {
            assert!(is_vite_trackable_path(p), "{p}");
        }
        for p in no {
            assert!(!is_vite_trackable_path(p), "{p}");
        }
    }
}
