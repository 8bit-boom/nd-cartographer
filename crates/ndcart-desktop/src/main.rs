//! The nd-cartographer desktop shell: a native window (Tauri v2) over the
//! same `ndcart` server the browser mode uses — one product, one renderer,
//! no second implementation. The server is bundled as a sidecar binary
//! (built from `crates/ndcart-server`); the shell picks a free port, starts
//! it on 127.0.0.1 with the user's data directory, waits for it to listen,
//! then opens the studio. Closing the app stops the server.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tauri::Manager;
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

/// The running server, stopped again when the app exits. The normal path is
/// the bundled sidecar (a plugin `CommandChild`); in development an `ndcart`
/// found on PATH is used instead (a plain std Child) — both are stopped the
/// same way. Interior mutability because the exit hook sees a shared state.
struct ServerGuard(std::sync::Mutex<ServerChild>);

struct ServerChild {
    plugin_child: Option<CommandChild>,
    std_child: Option<std::process::Child>,
}

impl ServerGuard {
    fn stop(&self) {
        let mut inner = self.0.lock().expect("server guard poisoned");
        if let Some(c) = inner.plugin_child.take() {
            let _ = c.kill(); // consumes the child
        }
        if let Some(mut c) = inner.std_child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn free_port() -> u16 {
    // Bind-and-drop is racy in theory and fine in practice: the server binds
    // immediately after and reports its own failure if the port was taken.
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(8307)
}

fn data_dir(app: &tauri::AppHandle) -> PathBuf {
    // Projects live with the user's other app data; ND_CARTO_DESKTOP_DATA
    // overrides it (a portable install on a stick).
    if let Ok(p) = std::env::var("ND_CARTO_DESKTOP_DATA") {
        return PathBuf::from(p);
    }
    match app.path().app_data_dir() {
        Ok(p) => p.join("data"),
        Err(_) => std::env::temp_dir().join("nd-cartographer-data"),
    }
}

/// Wait until something listens on `port` (the server binds only when ready
/// to serve), or `timeout` passes.
fn wait_for_listen(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn main() {
    let port = free_port();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(move |app| {
            let data = data_dir(app.handle());
            std::fs::create_dir_all(&data)
                .map_err(|e| format!("cannot create the data folder {}: {e}", data.display()))?;
            let listen = format!("127.0.0.1:{port}");
            let args = vec![
                "serve".to_string(),
                "--listen".to_string(),
                listen.clone(),
                "--data-dir".to_string(),
                data.to_string_lossy().into_owned(),
            ];

            let started = app.shell().sidecar("ndcart").and_then(|c| c.args(&args).spawn());
            let guard = match started {
                Ok((_events, plugin_child)) => ServerGuard(std::sync::Mutex::new(ServerChild {
                    plugin_child: Some(plugin_child),
                    std_child: None,
                })),
                Err(sidecar_err) => {
                    eprintln!("sidecar ndcart unavailable ({sidecar_err}); trying ndcart from PATH");
                    let std_child = std::process::Command::new("ndcart")
                        .args(&args)
                        .spawn()
                        .map_err(|e| {
                            format!("cannot start the ndcart server (sidecar: {sidecar_err}; PATH: {e})")
                        })?;
                    ServerGuard(std::sync::Mutex::new(ServerChild {
                        plugin_child: None,
                        std_child: Some(std_child),
                    }))
                }
            };
            app.manage(guard);
            if !wait_for_listen(port, Duration::from_secs(20)) {
                return Err("the ndcart server did not start in time".into());
            }
            let url = tauri::Url::parse(&format!("http://{listen}/")).map_err(|e| e.to_string())?;
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(url))
                .title("nd-cartographer")
                .inner_size(1400.0, 900.0)
                .min_inner_size(900.0, 600.0)
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building nd-cartographer")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(state) = app.try_state::<ServerGuard>() {
                    state.stop();
                }
            }
        });
}
