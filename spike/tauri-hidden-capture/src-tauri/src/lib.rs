// Spike for wayfinder ticket #17 (deanjstone/Hearth): does
// webkit_web_view_get_snapshot() produce real rendered pixels for a
// hidden/off-screen WebviewWindow, or blank/garbage? Undocumented upstream
// (tauri-apps/tao#289, open since 2022), no Wayland portal fallback exists
// for a windowless app — so the only way to know is to build the smallest
// thing that asks WebKitGTK directly.
//
// Runs two scenarios back to back, each against the same known HTML page,
// and writes each result to its own PNG for inspection:
//
//   A) "hidden-false"      — WebviewWindowBuilder::visible(false). This is
//      the naive approach: never map/realize the GTK widget at all.
//   B) "hidden-offscreen"  — window IS visible/mapped (so WebKit realizes a
//      real compositor surface) but positioned at (-32000, -32000): far
//      outside any real display, so nothing appears on the user's screen,
//      undecorated + skip-taskbar + unfocused so it doesn't steal focus or
//      show up in the taskbar/alt-tab either.
//
// See ../../README.md for the result of the last run.

use std::sync::{Arc, Mutex};
use tauri::{WebviewUrl, WebviewWindowBuilder};

const TEST_HTML: &str = r#"<!doctype html>
<html>
<head><style>
  html, body { margin: 0; height: 100%; background: #ff2d55; }
  h1 {
    margin: 0; height: 100%;
    display: flex; align-items: center; justify-content: center;
    color: #ffffff; font-family: sans-serif; font-size: 48px;
  }
</style></head>
<body><h1>HIDDEN CAPTURE TEST</h1></body>
</html>"#;

struct Scenario {
    label: &'static str,
    output_file: &'static str,
    visible: bool,
    offscreen: bool,
}

const SCENARIOS: [Scenario; 2] = [
    Scenario {
        label: "hidden-false",
        output_file: "snapshot-hidden-false.png",
        visible: false,
        offscreen: false,
    },
    Scenario {
        label: "hidden-offscreen",
        output_file: "snapshot-offscreen.png",
        visible: true,
        offscreen: true,
    },
];

fn output_path(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(file)
}

fn test_page_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("hearth-spike-hidden-capture-page.html");
    std::fs::write(&path, TEST_HTML).expect("failed to write test page");
    path
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let page_path = test_page_path();
            let results = Arc::new(Mutex::new(Vec::<(&'static str, String)>::new()));

            for scenario in &SCENARIOS {
                let url = WebviewUrl::External(
                    tauri::Url::from_file_path(&page_path).expect("valid file:// url"),
                );

                let app_handle = app.handle().clone();
                let results = results.clone();
                let label = scenario.label;
                let output_file = scenario.output_file;

                let mut builder = WebviewWindowBuilder::new(app, scenario.label, url)
                    .title(scenario.label)
                    .inner_size(800.0, 600.0)
                    .visible(scenario.visible);

                if scenario.offscreen {
                    builder = builder
                        .position(-32000.0, -32000.0)
                        .decorations(false)
                        .skip_taskbar(true)
                        .focused(false);
                }

                builder = builder.on_page_load(move |window, payload| {
                    if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                        return;
                    }
                    let app_handle = app_handle.clone();
                    let results = results.clone();
                    take_snapshot(window, output_file, move |outcome| {
                        println!("SCENARIO_RESULT[{label}]: {outcome}");
                        let mut results = results.lock().unwrap();
                        results.push((label, outcome));
                        if results.len() == SCENARIOS.len() {
                            app_handle.exit(0);
                        }
                    });
                });

                builder.build()?;
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(target_os = "linux")]
fn take_snapshot(
    window: tauri::WebviewWindow,
    output_file: &'static str,
    on_done: impl FnOnce(String) + Send + 'static,
) {
    use webkit2gtk::{gio, SnapshotOptions, SnapshotRegion, WebViewExt};

    window
        .with_webview(move |webview| {
            webview.inner().snapshot(
                SnapshotRegion::Visible,
                SnapshotOptions::empty(),
                gio::Cancellable::NONE,
                move |result| {
                    let outcome = match result {
                        Ok(surface) => match cairo::ImageSurface::try_from(surface) {
                            Ok(image) => write_png(&image, output_file),
                            Err(_) => "FAIL: surface was not an ImageSurface".to_string(),
                        },
                        Err(error) => format!("FAIL: snapshot error: {error}"),
                    };
                    on_done(outcome);
                },
            );
        })
        .expect("with_webview failed");
}

#[cfg(target_os = "linux")]
fn write_png(image: &cairo::ImageSurface, output_file: &str) -> String {
    let mut buf = Vec::new();
    match image.write_to_png(&mut buf) {
        Ok(()) => {
            let path = output_path(output_file);
            match std::fs::write(&path, &buf) {
                Ok(()) => format!(
                    "OK: wrote {} bytes ({}x{}) to {}",
                    buf.len(),
                    image.width(),
                    image.height(),
                    path.display()
                ),
                Err(error) => format!("FAIL: could not write PNG to disk: {error}"),
            }
        }
        Err(error) => format!("FAIL: PNG encode error: {error}"),
    }
}

#[cfg(not(target_os = "linux"))]
fn take_snapshot(
    _window: tauri::WebviewWindow,
    _output_file: &'static str,
    on_done: impl FnOnce(String) + Send + 'static,
) {
    on_done("SKIP: this spike only targets WebKitGTK (Linux)".to_string());
}
