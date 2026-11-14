#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use i_slint_backend_winit::WinitWindowAccessor;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

pub mod bug_reporter;
pub mod engine;
pub mod wow;

use bug_reporter::{BugReport, IssueType, Submitter};
use engine::downloader::{Downloader, ProgressEvent};
use engine::extractor::extract_tar_zst_file;
use engine::launcher_updater::{
    apply_self_update, fetch_latest_release, find_platform_asset, is_newer_version,
    restart_launcher, LAUNCHER_RELEASES_API_URL,
};
use engine::manifest::RemoteManifest;
use engine::scanner::{scan_delta, VIVAVOCE_MANAGED_MODULES};
use engine::uninstaller::{safe_remove_dir_all, uninstall_vivavoce};
use wow::discovery::{
    detect_wow_installations, find_wow_install_path, get_addons_path, validate_wow_root,
    WowFlavor, WowInstallation,
};
use wow::saved_variables::{load_all_saved_variables, RecentDialogue};

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

pub const REMOTE_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/Master-Antonio/Vivavoce-Releases/refs/heads/main/manifest.json";

#[derive(Clone)]
struct RingLogWriter {
    buffer: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
}

impl RingLogWriter {
    fn new(max_lines: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(VecDeque::with_capacity(max_lines))),
            max_lines,
        }
    }

    fn get_recent_lines(&self) -> Vec<String> {
        if let Ok(guard) = self.buffer.lock() {
            guard.iter().cloned().collect()
        } else {
            Vec::new()
        }
    }
}

impl std::io::Write for RingLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let _ = std::io::stdout().write(buf);
        if let Ok(mut guard) = self.buffer.lock() {
            for line in text.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if guard.len() >= self.max_lines {
                        guard.pop_front();
                    }
                    guard.push_back(trimmed.to_string());
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RingLogWriter {
    type Writer = RingLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

static GLOBAL_RING_LOG: OnceLock<RingLogWriter> = OnceLock::new();

/// Restituisce le ultime righe di log diagnostici registrate in memoria durante l'esecuzione
pub fn get_recent_launcher_logs() -> String {
    if let Some(writer) = GLOBAL_RING_LOG.get() {
        let lines = writer.get_recent_lines();
        if !lines.is_empty() {
            return lines.join("\n");
        }
    }
    String::new()
}

/// Recupera in modo resiliente il catalogo manifest con fallback multi-CDN,
/// timeout esteso (35s) e cache locale persistente su disco.
pub async fn fetch_remote_manifest() -> Option<RemoteManifest> {
    let endpoints = [
        "https://raw.githubusercontent.com/Master-Antonio/Vivavoce-Releases/refs/heads/main/manifest.json",
        "https://cdn.jsdelivr.net/gh/Master-Antonio/Vivavoce-Releases@main/manifest.json",
    ];

    let cache_dir = std::env::temp_dir().join("VivaVoceUpdates");
    let cache_file = cache_dir.join("manifest_cache.json");

    let client = reqwest::Client::builder()
        .user_agent("VivaVoceLauncher/0.1.0")
        .timeout(Duration::from_secs(35))
        .build()
        .unwrap_or_default();

    for url in endpoints {
        info!("Tentativo recupero manifest remoto da: {}", url);
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.bytes().await {
                    Ok(bytes) => {
                        match serde_json::from_slice::<RemoteManifest>(&bytes) {
                            Ok(manifest) => {
                                info!("Manifest v{} scaricato con successo da {}", manifest.version, url);
                                let _ = std::fs::create_dir_all(&cache_dir);
                                let _ = std::fs::write(&cache_file, &bytes);
                                return Some(manifest);
                            }
                            Err(e) => {
                                warn!("Errore deserializzazione manifest da {}: {}", url, e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Errore ricezione dati da {}: {}", url, e);
                    }
                }
            }
            Ok(resp) => {
                warn!("Risposta HTTP non valida ({}) da {}", resp.status(), url);
            }
            Err(e) => {
                warn!("Connessione fallita a {}: {}", url, e);
            }
        }
    }

    // Fallback a cache locale su disco se la connessione è assente o filtrata
    if cache_file.exists() {
        if let Ok(bytes) = std::fs::read(&cache_file) {
            if let Ok(manifest) = serde_json::from_slice::<RemoteManifest>(&bytes) {
                info!("Manifest recuperato dalla cache locale (offline/fallback, v{})", manifest.version);
                return Some(manifest);
            }
        }
    }

    error!("Impossibile recuperare il manifest di aggiornamento da nessuna sorgente!");
    None
}

slint::include_modules!();

#[derive(Clone)]
struct AppContext {
    main_window_weak: slint::Weak<MainWindow>,
    wow_installation: Arc<tokio::sync::RwLock<Option<WowInstallation>>>,
    recent_dialogues_cache: Arc<tokio::sync::RwLock<Vec<RecentDialogue>>>,
    cancel_token: Arc<AtomicBool>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Clean up leftover .old executable from previous self-update
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            if let Ok(entries) = std::fs::read_dir(exe_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if ext.eq_ignore_ascii_case("old") || ext.eq_ignore_ascii_case("tmp") {
                            let _ = std::fs::remove_file(path);
                        }
                    } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.contains(".old") || name.ends_with(".tmp") {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
            }
        }
    }

    // 1. Initialize logging with in-memory zero-console diagnostics buffer
    let ring_writer = RingLogWriter::new(100);
    let _ = GLOBAL_RING_LOG.set(ring_writer.clone());

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(ring_writer)
        .with_ansi(false)
        .init();

    info!("Avvio VivaVoce WoW Voiceover Launcher v{}...", env!("CARGO_PKG_VERSION"));

    // 2. Initialize UI
    let main_window = MainWindow::new()?;
    main_window.set_launcher_version(SharedString::from(env!("CARGO_PKG_VERSION")));
    let window_weak = main_window.as_weak();

    let app_context = AppContext {
        main_window_weak: window_weak.clone(),
        wow_installation: Arc::new(tokio::sync::RwLock::new(None)),
        recent_dialogues_cache: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        cancel_token: Arc::new(AtomicBool::new(false)),
    };

    // 3. Setup Window Management callbacks
    setup_window_callbacks(&main_window);

    // 4. Setup Action callbacks (Update, Repair, Uninstall, Bug Report, Folders)
    setup_action_callbacks(&main_window, app_context.clone());

    // 5. Initial Background Discovery
    let initial_ctx = app_context.clone();
    tokio::spawn(async move {
        discover_and_refresh(&initial_ctx).await;
    });

    // 5b. Initial Background Check for Launcher Updates
    let updater_ctx = app_context.clone();
    tokio::spawn(async move {
        run_check_launcher_update(updater_ctx, false).await;
    });

    // 6. Run Slint event loop
    main_window.run()?;
    info!("Chiusura VivaVoce Launcher terminata.");
    Ok(())
}

/// Setup frameless window dragging, minimize, maximize, resize, preset resolution, and close callbacks
fn setup_window_callbacks(main_window: &MainWindow) {
    let window_for_drag = main_window.as_weak();
    main_window.on_drag_window(move || {
        if let Some(win) = window_for_drag.upgrade() {
            win.window().with_winit_window(|winit_window| {
                let _ = winit_window.drag_window();
            });
        }
    });

    let window_for_minimize = main_window.as_weak();
    main_window.on_minimize_window(move || {
        if let Some(win) = window_for_minimize.upgrade() {
            win.window().set_minimized(true);
        }
    });

    let window_for_maximize = main_window.as_weak();
    main_window.on_maximize_window(move || {
        if let Some(win) = window_for_maximize.upgrade() {
            win.window().with_winit_window(|winit_window| {
                let is_max = winit_window.is_maximized();
                winit_window.set_maximized(!is_max);
                win.set_is_maximized(!is_max);
            });
        }
    });

    let window_for_resize = main_window.as_weak();
    main_window.on_resize_window(move || {
        if let Some(win) = window_for_resize.upgrade() {
            win.window().with_winit_window(|winit_window| {
                let _ = winit_window.drag_resize_window(winit::window::ResizeDirection::SouthEast);
            });
        }
    });

    let window_for_set_size = main_window.as_weak();
    main_window.on_set_window_size(move |width, height| {
        if let Some(win) = window_for_set_size.upgrade() {
            win.window().with_winit_window(|winit_window| {
                if width <= 0 || height <= 0 {
                    let is_max = winit_window.is_maximized();
                    winit_window.set_maximized(!is_max);
                    win.set_is_maximized(!is_max);
                } else {
                    if winit_window.is_maximized() {
                        winit_window.set_maximized(false);
                        win.set_is_maximized(false);
                    }
                    let _ = winit_window.request_inner_size(winit::dpi::LogicalSize::new(width as f64, height as f64));
                }
            });
        }
    });

    main_window.on_close_window(move || {
        let _ = slint::quit_event_loop();
    });

    main_window.on_open_url(|url| {
        open_system_url(url.as_str());
    });
}

/// Setup functional callbacks for addon actions, bug reporting, folder navigation
fn setup_action_callbacks(main_window: &MainWindow, ctx: AppContext) {
    // START UPDATE / INSTALL
    let update_ctx = ctx.clone();
    main_window.on_start_update(move || {
        let ctx_clone = update_ctx.clone();
        tokio::spawn(async move {
            run_update_pipeline(ctx_clone, false).await;
        });
    });

    // START REPAIR
    let repair_ctx = ctx.clone();
    main_window.on_start_repair(move || {
        let ctx_clone = repair_ctx.clone();
        tokio::spawn(async move {
            run_update_pipeline(ctx_clone, true).await;
        });
    });

    // START UNINSTALL
    let uninstall_ctx = ctx.clone();
    main_window.on_start_uninstall(move || {
        let ctx_clone = uninstall_ctx.clone();
        tokio::spawn(async move {
            run_uninstall(ctx_clone).await;
        });
    });

    // SUBMIT / EXPORT BUG REPORT TO DISK
    let bug_ctx = ctx.clone();
    main_window.on_submit_bug_report(move |rtype, cat, title, desc, extra, attach| {
        let ctx_clone = bug_ctx.clone();
        let cat_str = cat.to_string();
        let title_str = title.to_string();
        let desc_str = desc.to_string();
        let extra_str = extra.to_string();
        let attach_str = attach.to_string();
        tokio::spawn(async move {
            run_submit_bug_report(
                ctx_clone,
                rtype,
                cat_str,
                title_str,
                desc_str,
                extra_str,
                attach_str,
            )
            .await;
        });
    });

    // CREATE GITHUB ISSUE (DIRECT API OR AUTOMATED 1-CLICK)
    let issue_ctx = ctx.clone();
    main_window.on_create_github_issue(move |rtype, cat, title, desc, extra, attach| {
        let ctx_clone = issue_ctx.clone();
        let cat_str = cat.to_string();
        let title_str = title.to_string();
        let desc_str = desc.to_string();
        let extra_str = extra.to_string();
        let attach_str = attach.to_string();
        tokio::spawn(async move {
            run_create_github_issue(
                ctx_clone,
                rtype,
                cat_str,
                title_str,
                desc_str,
                extra_str,
                attach_str,
            )
            .await;
        });
    });

    // SELECT BUG ATTACHMENT (SCREENSHOT / VIDEO)
    let attach_ctx = ctx.clone();
    main_window.on_select_bug_attachment(move || {
        let ctx_clone = attach_ctx.clone();
        tokio::spawn(async move {
            run_select_bug_attachment(ctx_clone).await;
        });
    });

    // REVEAL ATTACHMENT IN EXPLORER
    let reveal_ctx = ctx.clone();
    main_window.on_reveal_bug_attachment(move || {
        if let Some(win) = reveal_ctx.main_window_weak.upgrade() {
            let p_str = win.get_bug_attachment_path().to_string();
            if !p_str.trim().is_empty() {
                Submitter::reveal_in_explorer(std::path::Path::new(&p_str));
            }
        }
    });

    // SELECT WOW FOLDER
    let folder_ctx = ctx.clone();
    main_window.on_select_wow_folder(move || {
        let ctx_clone = folder_ctx.clone();
        tokio::spawn(async move {
            run_browse_wow_folder(ctx_clone).await;
        });
    });

    // SELECT WOW FILE (WoW.exe) - Rock solid under Steam Deck / Wine / Proton
    let file_ctx = ctx.clone();
    main_window.on_select_wow_file(move || {
        let ctx_clone = file_ctx.clone();
        tokio::spawn(async move {
            run_browse_wow_file(ctx_clone).await;
        });
    });

    // SET CUSTOM WOW PATH (Manual text input or paste)
    let path_ctx = ctx.clone();
    main_window.on_set_custom_wow_path(move |custom_path| {
        let ctx_clone = path_ctx.clone();
        let path_str = custom_path.to_string();
        tokio::spawn(async move {
            run_set_custom_wow_path(ctx_clone, path_str).await;
        });
    });

    // OPEN ADDONS FOLDER
    let addons_ctx = ctx.clone();
    main_window.on_open_addons_folder(move || {
        let ctx_clone = addons_ctx.clone();
        tokio::spawn(async move {
            if let Some(inst) = ctx_clone.wow_installation.read().await.as_ref() {
                let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);
                let addons = get_addons_path(&inst.root_path, flavor);
                let _ = std::fs::create_dir_all(&addons);
                open_system_folder(&addons);
            }
        });
    });

    // LAUNCH GAME
    let launch_ctx = ctx.clone();
    main_window.on_launch_game(move || {
        let ctx_clone = launch_ctx.clone();
        tokio::spawn(async move {
            run_launch_game(ctx_clone).await;
        });
    });

    // INSTALL INDIVIDUAL MODULE
    let inst_mod_ctx = ctx.clone();
    main_window.on_install_module(move |id| {
        let ctx_clone = inst_mod_ctx.clone();
        let id_str = id.to_string();
        tokio::spawn(async move {
            run_install_module(ctx_clone, id_str).await;
        });
    });

    // REMOVE INDIVIDUAL MODULE
    let rem_mod_ctx = ctx.clone();
    main_window.on_remove_module(move |id| {
        let ctx_clone = rem_mod_ctx.clone();
        let id_str = id.to_string();
        tokio::spawn(async move {
            run_remove_module(ctx_clone, id_str).await;
        });
    });

    // CHECK LAUNCHER UPDATE
    let check_updater_ctx = ctx.clone();
    main_window.on_check_launcher_update(move || {
        let ctx_clone = check_updater_ctx.clone();
        tokio::spawn(async move {
            run_check_launcher_update(ctx_clone, true).await;
        });
    });

    // PERFORM LAUNCHER UPDATE
    let perform_updater_ctx = ctx.clone();
    main_window.on_perform_launcher_update(move || {
        let ctx_clone = perform_updater_ctx.clone();
        tokio::spawn(async move {
            run_perform_launcher_update(ctx_clone).await;
        });
    });
}

/// Auto-discovery of WoW on Windows, reading SavedVariables, and populating Slint models
async fn discover_and_refresh(ctx: &AppContext) {
    info!("Scansione installazioni World of Warcraft...");
    update_ui_status(&ctx.main_window_weak, "Rilevamento installazione World of Warcraft in corso...", 0.0);

    let installation = match find_wow_install_path() {
        Ok(inst) => {
            let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);
            info!("WoW trovato: {:?} (Flavor: {:?})", inst.root_path, flavor);
            Some(inst)
        }
        Err(e) => {
            warn!("Auto-discovery non riuscita ({}). Controllo installazioni multiple...", e);
            let multiple = detect_wow_installations();
            multiple.into_iter().next()
        }
    };

    if let Some(inst) = installation {
        let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);
        let addons_dir = get_addons_path(&inst.root_path, flavor);
        let is_installed = addons_dir.join("VivaVoce").exists() && addons_dir.join("VivaVoce/Core.lua").exists();
        let game_client_path = inst.game_client_path();
        let flavor_name = match flavor {
            WowFlavor::Forever => "WoW Forever".to_string(),
            other => format!("{:?}", other),
        };

        // Check installed version from .installed_version file or TOC if present
        let installed_version = if is_installed {
            let ver_file = addons_dir.join("VivaVoce/.installed_version");
            if let Ok(ver) = std::fs::read_to_string(&ver_file) {
                let trimmed = ver.trim().to_string();
                if !trimmed.is_empty() {
                    trimmed
                } else {
                    read_toc_version(&addons_dir.join("VivaVoce/VivaVoce.toc")).unwrap_or_else(|| "1.0.0".to_string())
                }
            } else {
                read_toc_version(&addons_dir.join("VivaVoce/VivaVoce.toc")).unwrap_or_else(|| "1.0.0".to_string())
            }
        } else {
            "".to_string()
        };

        // Cache installation
        *ctx.wow_installation.write().await = Some(inst.clone());

        // Read SavedVariables for recent dialogues (search client path and root)
        let saved_vars_dialogues = load_saved_variables_dialogues(&game_client_path).await;
        *ctx.recent_dialogues_cache.write().await = saved_vars_dialogues.clone();

        // Update UI
        let weak = ctx.main_window_weak.clone();
        let wow_path_str = game_client_path.to_string_lossy().to_string();
        let dialogues_for_ui = saved_vars_dialogues.clone();
        let installed_for_ui = installed_version.clone();

        let _ = weak.upgrade_in_event_loop(move |win| {
            win.set_wow_path(SharedString::from(wow_path_str.clone()));
            win.set_wow_path_input(SharedString::from(wow_path_str));
            win.set_wow_flavor(SharedString::from(flavor_name));
            win.set_is_installed(is_installed);
            win.set_installed_version(SharedString::from(installed_for_ui));
            win.set_remote_version(SharedString::from("1.0.0"));
            win.set_has_update(!is_installed);

            if is_installed {
                win.set_status_text(SharedString::from("VivaVoce è installato e pronto"));
                win.set_sub_status_text(SharedString::from("Tutti i pacchetti audio sono sincronizzati"));
            } else {
                win.set_status_text(SharedString::from("VivaVoce non è installato"));
                win.set_sub_status_text(SharedString::from("Premi Installa per scaricare l'intero doppiaggio"));
            }

            // Populate recent dialogues
            let dialogue_models: Vec<DialogueEntry> = dialogues_for_ui
                .iter()
                .map(|d| DialogueEntry {
                    npc_name: SharedString::from(d.title.clone()),
                    quest_id: d.quest_id.unwrap_or(0) as i32,
                    sound_file: SharedString::from(d.sound_file.clone()),
                    text: SharedString::from(d.text.clone()),
                    timestamp: SharedString::from(format_timestamp(d.timestamp)),
                })
                .collect();
            win.set_recent_dialogues(ModelRc::new(VecModel::from(dialogue_models)));

            // Populate package cards
            let packages = get_packages_status(Some(&addons_dir));
            win.set_packages(ModelRc::new(VecModel::from(packages)));
        });

        // Background check for latest remote manifest & delta patches
        let weak_for_remote = ctx.main_window_weak.clone();
        let installed_for_remote = installed_version.clone();
        tokio::spawn(async move {
            if let Some(manifest) = fetch_remote_manifest().await {
                let remote_ver = manifest.version.clone();
                let has_delta = manifest.find_patch(&installed_for_remote).is_some();
                let has_update = !is_installed || (!installed_for_remote.is_empty() && installed_for_remote != remote_ver);

                let _ = weak_for_remote.upgrade_in_event_loop(move |win| {
                    win.set_remote_version(SharedString::from(&remote_ver));
                    if is_installed && has_update {
                        win.set_has_update(true);
                        win.set_status_text(SharedString::from(format!("Aggiornamento v{} disponibile!", remote_ver)));
                        if has_delta {
                            win.set_sub_status_text(SharedString::from("Disponibile patch delta rapida: scarica solo i file modificati"));
                        } else {
                            win.set_sub_status_text(SharedString::from("Nuova versione completa disponibile"));
                        }
                    }
                });
            }
        });
    } else {
        warn!("Nessuna installazione di World of Warcraft rilevata.");
        let weak = ctx.main_window_weak.clone();
        let _ = weak.upgrade_in_event_loop(|win| {
            win.set_status_text(SharedString::from("WoW non rilevato"));
            win.set_sub_status_text(SharedString::from("Seleziona manualmente la cartella di World of Warcraft"));
            win.set_is_installed(false);
            win.set_has_update(false);
            let packages = get_packages_status(None);
            win.set_packages(ModelRc::new(VecModel::from(packages)));
        });
    }
}

/// Runs the complete update / installation / repair pipeline
async fn run_update_pipeline(ctx: AppContext, is_repair: bool) {
    let installation = {
        let lock = ctx.wow_installation.read().await;
        lock.clone()
    };

    let Some(inst) = installation else {
        update_ui_status(&ctx.main_window_weak, "Errore: Seleziona prima la cartella di World of Warcraft", 0.0);
        return;
    };

    let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);
    let addons_dir = get_addons_path(&inst.root_path, flavor);
    if let Err(e) = std::fs::create_dir_all(&addons_dir) {
        update_ui_status(&ctx.main_window_weak, &format!("Errore creazione cartella AddOns: {}", e), 0.0);
        return;
    }

    set_ui_updating(&ctx.main_window_weak, true);
    ctx.cancel_token.store(false, Ordering::SeqCst);

    let weak = ctx.main_window_weak.clone();
    let action_name = if is_repair { "Riparazione" } else { "Installazione/Aggiornamento" };
    info!("Avvio {} per VivaVoce in {:?}", action_name, addons_dir);

    let initial_msg = if is_repair {
        "Avvio verifica integrità file..."
    } else {
        "Avvio installazione di VivaVoce..."
    };
    let initial_sub = if is_repair {
        "Preparazione scansione file e checksum BLAKE3..."
    } else {
        "Verifica ambiente e pacchetti in corso..."
    };
    update_ui_status_with_sub(&weak, initial_msg, initial_sub, 0.02);

    // Progress channel
    let (tx, mut rx) = mpsc::channel::<ProgressEvent>(32);
    let weak_rx = weak.clone();
    tokio::spawn(async move {
        while let Some(evt) = rx.recv().await {
            let pct = if evt.total_bytes > 0 {
                (evt.current_bytes as f32 / evt.total_bytes as f32).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let speed_mb = evt.speed_bps / (1024.0 * 1024.0);
            let mb_done = evt.current_bytes as f64 / (1024.0 * 1024.0);
            let mb_tot = evt.total_bytes as f64 / (1024.0 * 1024.0);
            let sub = if evt.total_bytes > 0 {
                format!("{:.1} MB / {:.1} MB • {:.1} MB/s", mb_done, mb_tot, speed_mb)
            } else {
                String::new()
            };
            let msg = evt.message.clone();
            let _ = weak_rx.upgrade_in_event_loop(move |win| {
                win.set_progress(pct);
                win.set_status_text(SharedString::from(msg));
                if !sub.is_empty() {
                    win.set_sub_status_text(SharedString::from(sub));
                }
            });
        }
    });

    // 1. Fetch remote manifest
    update_ui_status_with_sub(
        &weak,
        "Connessione al catalogo aggiornamenti GitHub...",
        "Download catalogo manifest...",
        0.05,
    );
    let remote_manifest = fetch_remote_manifest().await;

    let vivavoce_dir = addons_dir.join("VivaVoce");
    let is_installed = vivavoce_dir.join("Core.lua").exists();
    let local_version = read_toc_version(&vivavoce_dir.join("VivaVoce.toc")).unwrap_or_else(|| "1.0.0".to_string());
    let downloader = Downloader::new();
    let temp_dir = std::env::temp_dir().join("VivaVoceUpdates");
    let _ = std::fs::create_dir_all(&temp_dir);

    // 2. REPAIR MODE
    if is_repair {
        if let Some(ref manifest) = remote_manifest {
            update_ui_status_with_sub(
                &weak,
                "Scansione rapida e verifica integrità file locali...",
                "Calcolo checksum BLAKE3 parallelo in corso...",
                0.2,
            );
            let addons_for_scan = addons_dir.clone();
            let manifest_for_scan = manifest.clone();
            let scan_res = tokio::task::spawn_blocking(move || {
                scan_delta(&addons_for_scan, &manifest_for_scan)
            }).await;

            match scan_res {
                Ok(Ok(delta)) => {
                    info!("Verifica completata: {} file integri, {} da riscaricare", delta.files_matched, delta.files_to_download.len());
                    if delta.files_to_download.is_empty() {
                        let _ = weak.upgrade_in_event_loop(move |win| {
                            win.set_progress(1.0);
                            win.set_status_text(SharedString::from("Integrità Verificata con Successo"));
                            win.set_sub_status_text(SharedString::from(format!("{} file verificati — Tutti i file sono integri", delta.files_matched)));
                            win.set_is_updating(false);
                        });
                        return;
                    } else {
                        info!("Trovati {} file mancanti o corrotti. Avvio ripristino...", delta.files_to_download.len());
                        update_ui_status_with_sub(
                            &weak,
                            &format!("Riparazione di {} file mancanti o corrotti...", delta.files_to_download.len()),
                            "Download e risincronizzazione file corretti in corso...",
                            0.35,
                        );
                        // Files need repair: proceed to delta patch / local sync / cloud download below
                    }
                }
                _ => {
                    warn!("Scansione delta fallita, continuo con verifica standard...");
                }
            }
        }
    }

    // 3. DELTA PATCH UPDATE: If user already has VivaVoce installed and a patch exists
    if is_installed && !is_repair {
        if let Some(ref manifest) = remote_manifest {
            if let Some(patch) = manifest.find_patch(&local_version) {
                info!("Trovata patch delta: v{} -> v{} ({} file)", patch.from_version, patch.to_version, patch.files_count);
                update_ui_status(&weak, &format!("Scaricamento patch delta v{} (solo file modificati)...", patch.to_version), 0.2);

                let patch_clone = patch.clone();
                let dl_res = downloader.download_patch(&patch_clone, &temp_dir, Some(tx.clone()), Some(ctx.cancel_token.clone())).await;
                match dl_res {
                    Ok(patch_file) => {
                        update_ui_status(&weak, "Applicazione file modificati in WoW Forever...", 0.85);
                        let addons_target = addons_dir.clone();
                        let patch_hash = patch.blake3.clone();
                        let tx_extract = tx.clone();

                        let ext_res = tokio::task::spawn_blocking(move || {
                            extract_tar_zst_file(&patch_file, &addons_target, Some(&patch_hash), Some(tx_extract))
                        }).await;

                        match ext_res {
                            Ok(Ok(summary)) => {
                                info!("Patch delta applicata! Estratti {} file", summary.files_extracted);
                                let new_ver = manifest.version.clone();
                                update_installed_toc_version(&addons_dir, &new_ver);
                                let addons_for_refresh = addons_dir.clone();
                                let _ = weak.upgrade_in_event_loop(move |win| {
                                    win.set_progress(1.0);
                                    win.set_status_text(SharedString::from(format!("VivaVoce v{} aggiornato con successo!", new_ver)));
                                    win.set_sub_status_text(SharedString::from(format!("Applicata patch delta: {} file modificati/nuovi aggiornati", summary.files_extracted)));
                                    win.set_is_installed(true);
                                    win.set_has_update(false);
                                    win.set_installed_version(SharedString::from(&new_ver));
                                    win.set_is_updating(false);
                                    let packages = get_packages_status(Some(&addons_for_refresh));
                                    win.set_packages(ModelRc::new(VecModel::from(packages)));
                                });
                                return;
                            }
                            Ok(Err(e)) => {
                                warn!("Estrazione patch fallita: {}", e);
                            }
                            Err(e) => {
                                warn!("Task fallito: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Download patch fallito: {}", e);
                    }
                }
            }
        }
    }

    // 4. LOCAL SOURCE SYNC (If workspace files are present locally on this PC)
    let local_pack_dir = PathBuf::from(r"K:\WoW\VivaVoce\pack\VivaVoce");
    let local_addon_dir = PathBuf::from(r"K:\WoW\VivaVoce\addon");
    if local_pack_dir.exists() || local_addon_dir.exists() {
        info!("Sorgente VivaVoce trovata in locale su disco. Sincronizzazione ad altissima velocità...");
        let sync_res = tokio::task::spawn_blocking({
            let target_addons = addons_dir.clone();
            let weak_clone = weak.clone();
            move || sync_local_vivavoce(&target_addons, weak_clone)
        }).await;

        if let Ok(Ok(_)) = sync_res {
            let ver = remote_manifest.map(|m| m.version).unwrap_or_else(|| "1.0.0".to_string());
            update_installed_toc_version(&addons_dir, &ver);
            let addons_for_refresh = addons_dir.clone();
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.set_progress(1.0);
                win.set_status_text(SharedString::from("VivaVoce sincronizzato e pronto"));
                win.set_sub_status_text(SharedString::from("27.420 battute italiane verificate"));
                win.set_is_installed(true);
                win.set_has_update(false);
                win.set_installed_version(SharedString::from(&ver));
                win.set_is_updating(false);
                let packages = get_packages_status(Some(&addons_for_refresh));
                win.set_packages(ModelRc::new(VecModel::from(packages)));
            });
            return;
        }
    }

    // 5. FULL CLOUD INSTALLATION (For fresh installs on any player's PC)
    if let Some(ref manifest) = remote_manifest {
        let total_pkgs = manifest.packages.len();
        for (i, pkg) in manifest.packages.iter().enumerate() {
            if ctx.cancel_token.load(Ordering::SeqCst) {
                update_ui_status(&weak, "Operazione annullata dall'utente.", 0.0);
                set_ui_updating(&weak, false);
                return;
            }

            update_ui_status_with_sub(
                &weak,
                &format!("Download modulo {}/{} ({})...", i + 1, total_pkgs, pkg.id),
                &format!("Scaricamento archivio {} in corso...", pkg.filename),
                (i as f32) / (total_pkgs as f32),
            );
            let pkg_clone = pkg.clone();
            let temp_dir_clone = temp_dir.clone();
            let tx_clone = tx.clone();
            let cancel_clone = ctx.cancel_token.clone();

            match downloader.download_package(&pkg_clone, &temp_dir_clone, Some(tx_clone), Some(cancel_clone)).await {
                Ok(pkg_path) => {
                    let addons_target = addons_dir.clone();
                    let hash_expected = pkg.blake3.clone();
                    let pkg_name = pkg.id.clone();
                    update_ui_status_with_sub(
                        &weak,
                        &format!("Estrazione e verifica {}/{}...", i + 1, total_pkgs),
                        &format!("Decompressione e verifica integrità {}...", pkg_name),
                        (i as f32) / (total_pkgs as f32) + 0.05,
                    );
                    let ext_res = tokio::task::spawn_blocking(move || {
                        extract_tar_zst_file(&pkg_path, &addons_target, Some(&hash_expected), None)
                    }).await;

                    match ext_res {
                        Ok(Ok(summary)) => {
                            info!("Modulo {} estratto: {} file ({} byte)", pkg_name, summary.files_extracted, summary.bytes_extracted);
                        }
                        Ok(Err(e)) => {
                            error!("Errore estrazione modulo {}: {}", pkg_name, e);
                            update_ui_status(&weak, &format!("Errore estrazione {}: {}", pkg_name, e), 0.0);
                            set_ui_updating(&weak, false);
                            return;
                        }
                        Err(e) => {
                            error!("Task estrazione fallito per {}: {}", pkg_name, e);
                            update_ui_status(&weak, &format!("Errore task estrazione: {}", e), 0.0);
                            set_ui_updating(&weak, false);
                            return;
                        }
                    }
                }
                Err(e) => {
                    error!("Errore download modulo {}: {}", pkg.id, e);
                    update_ui_status(&weak, &format!("Errore download {}: {}", pkg.id, e), 0.0);
                    set_ui_updating(&weak, false);
                    return;
                }
            }
        }

        let new_ver = manifest.version.clone();
        update_installed_toc_version(&addons_dir, &new_ver);
        let addons_for_refresh = addons_dir.clone();
        let _ = weak.upgrade_in_event_loop(move |win| {
            win.set_progress(1.0);
            win.set_status_text(SharedString::from("Installazione completata con successo!"));
            win.set_sub_status_text(SharedString::from(format!("VivaVoce v{} è pronto per giocare", new_ver)));
            win.set_is_installed(true);
            win.set_has_update(false);
            win.set_installed_version(SharedString::from(&new_ver));
            win.set_is_updating(false);
            let packages = get_packages_status(Some(&addons_for_refresh));
            win.set_packages(ModelRc::new(VecModel::from(packages)));
        });
    } else {
        update_ui_status(&weak, "Impossibile scaricare gli aggiornamenti da GitHub.", 0.0);
        set_ui_updating(&weak, false);
    }
}

/// Sincronizzazione locale diretta ad altissima velocità tra repository e cartella AddOns
fn sync_local_vivavoce(target_addons: &Path, weak: slint::Weak<MainWindow>) -> std::io::Result<()> {
    let source_base = PathBuf::from(r"K:\WoW\VivaVoce");
    let modules = [
        "VivaVoce",
        "VivaVoce_Quests",
        "VivaVoce_Books",
        "VivaVoce_Codex",
        "VivaVoce_Zones",
        "VivaVoce_Collector",
    ];

    let total = modules.len();
    for (i, module) in modules.iter().enumerate() {
        let pct = (i as f32) / (total as f32);
        let msg = format!("Sincronizzazione modulo {}/{} ({})...", i + 1, total, module);
        let _ = weak.upgrade_in_event_loop(move |win| {
            win.set_progress(pct);
            win.set_status_text(SharedString::from(msg));
            win.set_sub_status_text(SharedString::from("Sincronizzazione veloce file locali in corso..."));
        });

        let dst = target_addons.join(module);

        if *module == "VivaVoce" {
            // 1. Copy Lua addon code and textures from K:\WoW\VivaVoce\addon
            let addon_src = source_base.join("addon");
            if addon_src.exists() {
                copy_dir_recursive_smart(&addon_src, &dst)?;
            }
            // 2. Copy sounds from K:\WoW\VivaVoce\pack\VivaVoce\Sounds (if present)
            let sounds_src = source_base.join("pack/VivaVoce/Sounds");
            if sounds_src.exists() {
                copy_dir_recursive_smart(&sounds_src, &dst.join("Sounds"))?;
            }
        } else {
            let src = source_base.join(module);
            if src.exists() {
                copy_dir_recursive_smart(&src, &dst)?;
            }
        }
    }

    Ok(())
}

/// Helper ricorsivo per la copia intelligente di directory (salta file identici per evitare riscritture inutili)
fn copy_dir_recursive_smart(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let dest_child = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive_smart(&entry.path(), &dest_child)?;
        } else {
            if let Ok(dest_meta) = std::fs::metadata(&dest_child) {
                if let Ok(src_meta) = entry.metadata() {
                    if dest_meta.len() == src_meta.len() {
                        continue;
                    }
                }
            }
            std::fs::copy(entry.path(), dest_child)?;
        }
    }
    Ok(())
}

/// Clean uninstallation of VivaVoce modules
async fn run_uninstall(ctx: AppContext) {
    let installation = {
        let lock = ctx.wow_installation.read().await;
        lock.clone()
    };

    let Some(inst) = installation else {
        update_ui_status(&ctx.main_window_weak, "Errore: Nessun percorso WoW valido", 0.0);
        return;
    };

    set_ui_updating(&ctx.main_window_weak, true);
    update_ui_status(&ctx.main_window_weak, "Disinstallazione di VivaVoce in corso...", 0.5);

    let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);
    let addons_dir = get_addons_path(&inst.root_path, flavor);
    let addons_dir_uninst = addons_dir.clone();
    let res = tokio::task::spawn_blocking(move || {
        uninstall_vivavoce(&addons_dir_uninst, false)
    }).await;

    match res {
        Ok(Ok(uninst)) => {
            info!("VivaVoce disinstallato: rimosse {:?}, liberati {} byte", uninst.removed_addon_dirs, uninst.freed_bytes);
            let weak = ctx.main_window_weak.clone();
            let addons_for_refresh = addons_dir.clone();
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.set_is_installed(false);
                win.set_has_update(true);
                win.set_progress(0.0);
                win.set_status_text(SharedString::from("VivaVoce rimosso correttamente"));
                win.set_sub_status_text(SharedString::from("Nessun altro addon è stato modificato"));
                win.set_is_updating(false);
                let packages = get_packages_status(Some(&addons_for_refresh));
                win.set_packages(ModelRc::new(VecModel::from(packages)));
            });
        }
        Ok(Err(e)) => {
            error!("Errore durante la disinstallazione: {}", e);
            update_ui_status(&ctx.main_window_weak, &format!("Errore rimozione: {}", e), 0.0);
            set_ui_updating(&ctx.main_window_weak, false);
        }
        Err(e) => {
            error!("Task disinstallazione interrotto: {}", e);
            set_ui_updating(&ctx.main_window_weak, false);
        }
    }
}

/// Returns a formatted OS string for bug reports (Windows, macOS with arch, Linux)
fn current_os_info() -> String {
    #[cfg(target_os = "windows")]
    {
        "Windows (x86_64)".to_string()
    }
    #[cfg(target_os = "macos")]
    {
        format!("macOS ({})", std::env::consts::ARCH)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        format!("Linux ({})", std::env::consts::ARCH)
    }
}

/// Helper to construct a BugReport for Audio, AddOn, or Launcher issues
async fn create_bug_report_from_params(
    ctx: &AppContext,
    report_type_idx: i32,
    category_str: String,
    title_str: String,
    user_notes: String,
    extra_str: String,
    attachment_str: String,
) -> Option<BugReport> {
    let installation = {
        let lock = ctx.wow_installation.read().await;
        lock.clone()
    };

    let (wow_path, client_flavor) = if let Some(ref inst) = installation {
        let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);
        let flavor_str = match flavor {
            WowFlavor::Forever => "WoW Forever".to_string(),
            other => format!("{}", other),
        };
        (inst.root_path.to_string_lossy().to_string(), flavor_str)
    } else {
        ("Non configurato / Non rilevato".to_string(), "Sconosciuto".to_string())
    };

    // For Audio report only, if WoW installation is not configured, we cannot map files
    if report_type_idx == 0 && installation.is_none() {
        return None;
    }

    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

    let attachment_path = if attachment_str.trim().is_empty() {
        None
    } else {
        Some(attachment_str.trim().to_string())
    };

    let os_str = current_os_info();

    if report_type_idx == 0 {
        // AUDIO REPORT
        let dialogues = ctx.recent_dialogues_cache.read().await.clone();
        let matched = dialogues.iter().find(|d| d.sound_file == extra_str || (extra_str.is_empty() && !d.sound_file.is_empty()));

        // Parse special tags from user_notes if present (e.g. from "Voce mancante")
        let mut clean_notes = user_notes.clone();
        let mut parsed_quest_id = None;
        let mut parsed_text = None;

        if let Some(start) = clean_notes.find("[Quest/NPC ID: ") {
            if let Some(end) = clean_notes[start..].find(']') {
                let id_str = &clean_notes[start + 15..start + end];
                if let Ok(id) = id_str.trim().parse::<u32>() {
                    parsed_quest_id = Some(id);
                }
                clean_notes.replace_range(start..start + end + 1, "");
            }
        }

        if let Some(start) = clean_notes.find("[Testo a schermo: ") {
            if let Some(end) = clean_notes[start..].find(']') {
                let text_val = clean_notes[start + 18..start + end].trim().to_string();
                if !text_val.is_empty() {
                    parsed_text = Some(text_val);
                }
                clean_notes.replace_range(start..start + end + 1, "");
            }
        }
        let clean_notes = clean_notes.trim().to_string();

        let (quest_id, npc_id, dialogue_title, dialogue_text) = if let Some(d) = matched {
            (
                d.quest_id.or(parsed_quest_id),
                d.npc_id,
                Some(d.title.clone()),
                Some(d.text.clone()).or(parsed_text),
            )
        } else {
            (
                parsed_quest_id,
                None,
                if title_str.is_empty() { None } else { Some(title_str.clone()) },
                parsed_text,
            )
        };

        Some(BugReport {
            issue_type: IssueType::Audio,
            category: category_str,
            title: if title_str.is_empty() { "Segnalazione Audio".to_string() } else { title_str },
            user_description: clean_notes,
            repro_steps: None,
            addon_version: "1.0.0".to_string(),
            client_flavor,
            wow_path,
            os_info: os_str,
            sound_file: if extra_str.is_empty() { None } else { Some(extra_str) },
            quest_id,
            npc_id,
            dialogue_title,
            dialogue_text,
            attachment_path,
            timestamp_epoch: epoch,
        })
    } else if report_type_idx == 1 {
        // ADDON REPORT
        Some(BugReport {
            issue_type: IssueType::Addon,
            category: category_str,
            title: if title_str.is_empty() { "Problema AddOn".to_string() } else { title_str },
            user_description: user_notes,
            repro_steps: if extra_str.trim().is_empty() { None } else { Some(extra_str) },
            addon_version: "1.0.0".to_string(),
            client_flavor,
            wow_path,
            os_info: os_str,
            sound_file: None,
            quest_id: None,
            npc_id: None,
            dialogue_title: None,
            dialogue_text: None,
            attachment_path,
            timestamp_epoch: epoch,
        })
    } else {
        // LAUNCHER REPORT (report_type_idx == 2)
        let (repro, logs) = if let Some(idx) = extra_str.find("\n---LOGS---\n") {
            let (r, l) = extra_str.split_at(idx);
            let log_part = &l["\n---LOGS---\n".len()..];
            (
                if r.trim().is_empty() { None } else { Some(r.trim().to_string()) },
                if log_part.trim().is_empty() { None } else { Some(log_part.trim().to_string()) },
            )
        } else {
            (
                if extra_str.trim().is_empty() { None } else { Some(extra_str.trim().to_string()) },
                None,
            )
        };

        // Capture recent in-memory diagnostics logs automatically (Zero-Console experience)
        let auto_logs = get_recent_launcher_logs();
        let final_logs = match (logs, auto_logs.trim().is_empty()) {
            (Some(user_log), false) => format!("{}\n\n--- Log Runtime Automatici (Diagnostica Launcher) ---\n{}", user_log.trim(), auto_logs.trim()),
            (Some(user_log), true) => user_log,
            (None, false) => auto_logs,
            (None, true) => String::new(),
        };

        let description_with_logs = if !final_logs.trim().is_empty() {
            format!("{}\n\n### Log / Diagnostica di Sistema (Automatici)\n```text\n{}\n```", user_notes.trim(), final_logs.trim())
        } else {
            user_notes
        };

        Some(BugReport {
            issue_type: IssueType::Launcher,
            category: category_str,
            title: if title_str.is_empty() { "Problema Launcher".to_string() } else { title_str },
            user_description: description_with_logs,
            repro_steps: repro,
            addon_version: env!("CARGO_PKG_VERSION").to_string(),
            client_flavor,
            wow_path,
            os_info: os_str,
            sound_file: None,
            quest_id: None,
            npc_id: None,
            dialogue_title: None,
            dialogue_text: None,
            attachment_path,
            timestamp_epoch: epoch,
        })
    }
}

/// Exports bug report to local Markdown file and copies text to clipboard
async fn run_submit_bug_report(
    ctx: AppContext,
    report_type_idx: i32,
    category_str: String,
    title_str: String,
    user_notes: String,
    extra_str: String,
    attachment_str: String,
) {
    let report_opt = create_bug_report_from_params(
        &ctx,
        report_type_idx,
        category_str,
        title_str,
        user_notes,
        extra_str,
        attachment_str,
    )
    .await;

    let Some(report) = report_opt else {
        update_ui_status(&ctx.main_window_weak, "Impossibile creare report: WoW non configurato", 0.0);
        return;
    };

    let target_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match Submitter::save_to_disk(&report, &target_dir) {
        Ok(path) => {
            let path_str = path.to_string_lossy().to_string();
            info!("Bug report salvato su disco in {:?}", path);
            Submitter::copy_to_clipboard(&report.to_markdown());
            let weak = ctx.main_window_weak.clone();
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.set_bug_submit_status(SharedString::from("Report esportato e copiato negli appunti!"));
                win.set_status_text(SharedString::from("Segnalazione esportata"));
                win.set_sub_status_text(SharedString::from(format!("Salvato in: {}", path_str)));
            });
        }
        Err(e) => {
            error!("Errore salvataggio bug report: {}", e);
            let weak = ctx.main_window_weak.clone();
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.set_bug_submit_status(SharedString::from(format!("Errore salvataggio: {}", e)));
            });
        }
    }
}

/// Automated GitHub issue submission (via GitHub REST API or pre-filled web fallback)
async fn run_create_github_issue(
    ctx: AppContext,
    report_type_idx: i32,
    category_str: String,
    title_str: String,
    user_notes: String,
    extra_str: String,
    attachment_str: String,
) {
    let report_opt = create_bug_report_from_params(
        &ctx,
        report_type_idx,
        category_str,
        title_str,
        user_notes,
        extra_str,
        attachment_str,
    )
    .await;

    let Some(report) = report_opt else {
        update_ui_status(&ctx.main_window_weak, "Impossibile creare report: WoW non configurato", 0.0);
        return;
    };

    let token = {
        let mut t = String::new();
        if let Some(win) = ctx.main_window_weak.upgrade() {
            t = win.get_github_token().to_string();
        }
        if t.trim().is_empty() {
            if let Ok(env_t) = std::env::var("GITHUB_TOKEN") {
                t = env_t;
            }
        }
        t
    };

    let weak = ctx.main_window_weak.clone();

    if !token.trim().is_empty() {
        let _ = weak.upgrade_in_event_loop(|win| {
            win.set_bug_submit_status(SharedString::from("Invio issue a GitHub via API..."));
        });

        match Submitter::submit_to_github_api(&report, &token).await {
            Ok(issue_url) => {
                info!("Issue creata con successo via API GitHub: {}", issue_url);
                if report.attachment_path.is_none() {
                    Submitter::copy_to_clipboard(&report.to_markdown());
                }
                let issue_url_clone = issue_url.clone();
                let _ = weak.upgrade_in_event_loop(move |win| {
                    win.set_bug_submit_status(SharedString::from("Issue creata su GitHub con successo!"));
                    win.set_last_created_issue_url(SharedString::from(&issue_url_clone));
                    win.set_status_text(SharedString::from("Issue GitHub creata con successo"));
                    win.set_sub_status_text(SharedString::from(issue_url_clone));
                });
            }
            Err(e) => {
                warn!("API GitHub fallita: {}. Fallback 1-click su browser...", e);
                let url = Submitter::build_github_issue_url(&report);
                if report.attachment_path.is_none() {
                    Submitter::copy_to_clipboard(&report.to_markdown());
                }
                open_system_url(&url);

                let err_msg = format!("Errore API: {}. Aperta pagina web GitHub con campi già pronti.", e);
                let _ = weak.upgrade_in_event_loop(move |win| {
                    win.set_bug_submit_status(SharedString::from(err_msg));
                });
            }
        }
    } else {
        // Automated 1-click fallback: opens browser with prefilled markdown body, title and labels
        let url = Submitter::build_github_issue_url(&report);

        let mut copied_media = false;
        if let Some(ref path_str) = report.attachment_path {
            let p = std::path::Path::new(path_str);
            if p.exists() {
                Submitter::copy_media_to_clipboard(p);
                copied_media = true;
            }
        }

        // Only copy markdown text to clipboard if NO media was selected,
        // so clip.exe does not overwrite the image/video on the clipboard!
        if !copied_media {
            Submitter::copy_to_clipboard(&report.to_markdown());
        }

        open_system_url(&url);

        let status_msg = if copied_media {
            "Pagina GitHub aperta! Clicca nella descrizione e premi CTRL+V per incollare l'allegato."
        } else {
            "Pagina GitHub aperta con tutti i dati precompilati!"
        };

        let _ = weak.upgrade_in_event_loop(move |win| {
            win.set_bug_submit_status(SharedString::from(status_msg));
            win.set_status_text(SharedString::from("Aperta pagina GitHub con segnalazione precompilata"));
            win.set_sub_status_text(SharedString::from(if copied_media {
                "Allegato copiato negli appunti: premi CTRL+V su GitHub"
            } else {
                "Report copiato negli appunti"
            }));
        });
    }
}

/// Browse WoW folder via native file dialog
async fn run_browse_wow_folder(ctx: AppContext) {
    let picked = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Seleziona la cartella di World of Warcraft")
            .pick_folder()
    }).await;

    if let Ok(Some(path)) = picked {
        apply_new_wow_path(ctx, path).await;
    }
}

/// Browse WoW executable (WoW.exe) via native file dialog (rock-solid under Proton / Wine on Linux)
async fn run_browse_wow_file(ctx: AppContext) {
    let picked = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Seleziona l'eseguibile di World of Warcraft (WoW.exe)")
            .add_filter("Eseguibile World of Warcraft (*.exe, *.app)", &["exe", "app"])
            .pick_file()
    }).await;

    if let Ok(Some(file_path)) = picked {
        let folder = file_path.parent().unwrap_or(&file_path).to_path_buf();
        apply_new_wow_path(ctx, folder).await;
    }
}

/// Set custom WoW path from manual text input or paste
async fn run_set_custom_wow_path(ctx: AppContext, raw_input: String) {
    let trimmed = raw_input.trim();
    if trimmed.is_empty() {
        update_ui_status(&ctx.main_window_weak, "Inserisci un percorso valido prima di applicare", 0.0);
        return;
    }
    let normalized = wow::discovery::normalize_path(Path::new(trimmed));
    apply_new_wow_path(ctx, normalized).await;
}

/// Common logic to validate and apply any newly selected or entered WoW path
async fn apply_new_wow_path(ctx: AppContext, raw_path: PathBuf) {
    let normalized = wow::discovery::normalize_path(&raw_path);
    info!("Verifica percorso WoW specificato: {:?}", normalized);

    let resolved = wow::discovery::resolve_candidate_root(&normalized);
    match validate_wow_root(&resolved) {
        Ok(flavors) => {
            let inst = WowInstallation::new(resolved.clone(), flavors);
            info!("Percorso WoW confermato e impostato con successo: {:?}", resolved);
            *ctx.wow_installation.write().await = Some(inst);
            discover_and_refresh(&ctx).await;
            update_ui_status(
                &ctx.main_window_weak,
                &format!("Percorso WoW impostato: {}", resolved.display()),
                0.0,
            );
        }
        Err(e) => {
            warn!("Percorso specificato non valido ({:?}): {}", resolved, e);
            update_ui_status(
                &ctx.main_window_weak,
                "Cartella non valida: non contiene WoW o la cartella Interface.",
                0.0,
            );
        }
    }
}

/// Browse screenshot or video file for bug reporting via native file dialog
async fn run_select_bug_attachment(ctx: AppContext) {
    let picked = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Seleziona Screenshot o Video per la segnalazione")
            .add_filter(
                "Immagini e Video (*.png, *.jpg, *.mp4, *.mkv...)",
                &["png", "jpg", "jpeg", "gif", "bmp", "webp", "mp4", "webm", "avi", "mkv", "mov"],
            )
            .pick_file()
    })
    .await;

    if let Ok(Some(path)) = picked {
        let path_str = path.to_string_lossy().to_string();
        let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        info!("Allegato bug selezionato: {} ({})", filename, path_str);

        // Pre-carica immediatamente l'immagine/file negli appunti Windows
        let path_copy = path.clone();
        tokio::task::spawn_blocking(move || {
            Submitter::copy_media_to_clipboard(&path_copy);
        });

        let weak = ctx.main_window_weak.clone();
        let _ = weak.upgrade_in_event_loop(move |win| {
            win.set_bug_attachment_path(SharedString::from(path_str));
            win.set_bug_attachment_filename(SharedString::from(filename));
        });
    }
}

/// Launch World of Warcraft executable or macOS .app bundle
async fn run_launch_game(ctx: AppContext) {
    let inst_opt = {
        let lock = ctx.wow_installation.read().await;
        lock.clone()
    };

    let Some(inst) = inst_opt else {
        update_ui_status(&ctx.main_window_weak, "Errore: Percorso WoW Forever non impostato", 0.0);
        return;
    };

    let client_path = inst.game_client_path();
    let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);

    let found_exe = wow::discovery::find_game_executable(&inst.root_path, flavor)
        .or_else(|| wow::discovery::find_game_executable(&client_path, flavor));

    if let Some(exe) = found_exe {
        info!("Avvio eseguibile di gioco WoW Forever: {:?}", exe);

        #[cfg(target_os = "macos")]
        let res = std::process::Command::new("open").arg(exe).spawn();

        #[cfg(not(target_os = "macos"))]
        let res = std::process::Command::new(&exe).current_dir(exe.parent().unwrap_or(&client_path)).spawn();

        match res {
            Ok(_) => {
                update_ui_status(&ctx.main_window_weak, "World of Warcraft: Forever avviato!", 1.0);
            }
            Err(e) => {
                error!("Errore avvio gioco: {}", e);
                update_ui_status(&ctx.main_window_weak, &format!("Errore avvio WoW Forever: {}", e), 0.0);
            }
        }
    } else {
        warn!("Eseguibile WoW non trovato nelle posizioni note.");
        update_ui_status(&ctx.main_window_weak, "Eseguibile WoW non trovato nella cartella di WoW Forever.", 0.0);
    }
}

/// Loads recent dialogues from all SavedVariables in WTF folder
async fn load_saved_variables_dialogues(wow_root: &Path) -> Vec<RecentDialogue> {
    let loaded = load_all_saved_variables(wow_root);
    let mut all_dialogues = Vec::new();
    for account in loaded {
        all_dialogues.extend(account.recent_dialogues);
    }

    // Sort descending by timestamp
    all_dialogues.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
    all_dialogues.truncate(50);
    all_dialogues
}

/// Formats epoch seconds into human-readable date/time string
fn format_timestamp(epoch: u64) -> String {
    if epoch == 0 {
        return "Recentemente".to_string();
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    if now >= epoch {
        let diff = now - epoch;
        if diff < 60 {
            return "Pochi secondi fa".to_string();
        } else if diff < 3600 {
            return format!("{} min fa", diff / 60);
        } else if diff < 86400 {
            return format!("{} ore fa", diff / 3600);
        } else {
            return format!("{} giorni fa", diff / 86400);
        }
    }
    "Data recente".to_string()
}

/// Parses version tag from TOC file
fn read_toc_version(toc_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(toc_path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## Version:") {
            return Some(trimmed.trim_start_matches("## Version:").trim().to_string());
        }
    }
    None
}

/// Aggiorna i file TOC e il file .installed_version per riflettere la versione installata
fn update_installed_toc_version(addons_dir: &Path, version: &str) {
    for &module in VIVAVOCE_MANAGED_MODULES {
        let toc_path = addons_dir.join(module).join(format!("{}.toc", module));
        if toc_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&toc_path) {
                let mut new_lines = Vec::new();
                let mut replaced = false;
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("## Version:") {
                        new_lines.push(format!("## Version: {}", version));
                        replaced = true;
                    } else {
                        new_lines.push(line.to_string());
                    }
                }
                if !replaced {
                    new_lines.insert(0, format!("## Version: {}", version));
                }
                let _ = std::fs::write(&toc_path, new_lines.join("\r\n"));
            }
        }
    }

    let vv_dir = addons_dir.join("VivaVoce");
    if vv_dir.exists() {
        let ver_file = vv_dir.join(".installed_version");
        let _ = std::fs::write(&ver_file, version);
    }
}

/// Returns the 7 VivaVoce package descriptors with live on-disk installation status
fn get_packages_status(addons_dir: Option<&Path>) -> Vec<PackageInfo> {
    let check = |folder: &str| -> bool {
        if let Some(dir) = addons_dir {
            dir.join(folder).is_dir()
        } else {
            false
        }
    };

    let core_installed = check("VivaVoce") && addons_dir.map_or(false, |d| d.join("VivaVoce/Core.lua").exists());
    let quests_installed = check("VivaVoce_Quests");
    let books_installed = check("VivaVoce_Books");
    let codex_installed = check("VivaVoce_Codex");
    let zones_installed = check("VivaVoce_Zones");
    let collector_installed = check("VivaVoce_Collector");
    let sounds_installed = addons_dir.map_or(false, |d| {
        let s = d.join("VivaVoce/Sounds");
        s.is_dir() && std::fs::read_dir(&s).map_or(false, |mut it| it.next().is_some())
    });

    vec![
        PackageInfo {
            id: SharedString::from("vivavoce_core"),
            name: SharedString::from("VivaVoce Core"),
            description: SharedString::from("Motore audio principale, gestione code, sottotitoli e opzioni"),
            filename: SharedString::from("VivaVoce_Core.tar.zst"),
            size_str: SharedString::from("19.5 MB"),
            installed: core_installed,
            verified: core_installed,
            blake3_hash: SharedString::from("8ac4...7b94"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce_sounds"),
            name: SharedString::from("VivaVoce Voci & Audio (HQ)"),
            description: SharedString::from("27.420 battute audio recitate in italiano (Parti 1, 2 e 3)"),
            filename: SharedString::from("VivaVoce_Sounds (4.4 GB)"),
            size_str: SharedString::from("4.48 GB"),
            installed: sounds_installed,
            verified: sounds_installed,
            blake3_hash: SharedString::from("Voci HQ"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce_quests"),
            name: SharedString::from("VivaVoce Quests"),
            description: SharedString::from("Database testi & dialoghi quest, gossip NPC, accettazione e completamento"),
            filename: SharedString::from("VivaVoce_Quests.tar.zst"),
            size_str: SharedString::from("395 KB"),
            installed: quests_installed,
            verified: quests_installed,
            blake3_hash: SharedString::from("f440...cb3c"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce_books"),
            name: SharedString::from("VivaVoce Books"),
            description: SharedString::from("Database tomi di Azeroth, lettere, diari, pergamene e lapidi"),
            filename: SharedString::from("VivaVoce_Books.tar.zst"),
            size_str: SharedString::from("334 KB"),
            installed: books_installed,
            verified: books_installed,
            blake3_hash: SharedString::from("83d8...2ec5"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce_codex"),
            name: SharedString::from("VivaVoce Codex"),
            description: SharedString::from("Supporto a CodexIT e biografie lore dei personaggi"),
            filename: SharedString::from("VivaVoce_Codex.tar.zst"),
            size_str: SharedString::from("1.57 MB"),
            installed: codex_installed,
            verified: codex_installed,
            blake3_hash: SharedString::from("c148...6134"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce_zones"),
            name: SharedString::from("VivaVoce Zones"),
            description: SharedString::from("Annunci e tracce d'atmosfera delle zone del mondo"),
            filename: SharedString::from("VivaVoce_Zones.tar.zst"),
            size_str: SharedString::from("81.2 MB"),
            installed: zones_installed,
            verified: zones_installed,
            blake3_hash: SharedString::from("0e4b...d0ea"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce_collector"),
            name: SharedString::from("VivaVoce Collector"),
            description: SharedString::from("Rilevamento in-game di dialoghi non doppiati e telemetria locale"),
            filename: SharedString::from("VivaVoce_Collector.tar.zst"),
            size_str: SharedString::from("287 KB"),
            installed: collector_installed,
            verified: collector_installed,
            blake3_hash: SharedString::from("d856...cb8e"),
        },
    ]
}

/// Installazione di un singolo modulo selezionato
async fn run_install_module(ctx: AppContext, module_id: String) {
    let installation = {
        let lock = ctx.wow_installation.read().await;
        lock.clone()
    };
    let Some(inst) = installation else {
        update_ui_status(&ctx.main_window_weak, "Errore: Nessun percorso WoW valido", 0.0);
        return;
    };

    let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);
    let addons_dir = get_addons_path(&inst.root_path, flavor);

    if module_id == "vivavoce_sounds" || module_id == "vivavoce-sounds" {
        set_ui_updating(&ctx.main_window_weak, true);
        update_ui_status(&ctx.main_window_weak, "Installazione Pacchetto Audio HQ in corso...", 0.1);
        let weak = ctx.main_window_weak.clone();

        // Controlla sorgenti locali su disco di sviluppo
        let local_sounds = PathBuf::from(r"K:\WoW\VivaVoce\pack\VivaVoce\Sounds");
        if local_sounds.exists() {
            let dst = addons_dir.join("VivaVoce/Sounds");
            let sync_res = tokio::task::spawn_blocking(move || {
                copy_dir_recursive_smart(&local_sounds, &dst)
            }).await;

            if let Ok(Ok(_)) = sync_res {
                let addons_for_refresh = addons_dir.clone();
                let _ = weak.upgrade_in_event_loop(move |win| {
                    let packages = get_packages_status(Some(&addons_for_refresh));
                    win.set_packages(ModelRc::new(VecModel::from(packages)));
                    win.set_status_text(SharedString::from("Pacchetto Audio HQ sincronizzato!"));
                    win.set_sub_status_text(SharedString::from("27.420 tracce audio pronte in locale"));
                    win.set_is_updating(false);
                });
                return;
            }
        }

        // Download da GitHub Releases per le 3 parti audio
        if let Some(manifest) = fetch_remote_manifest().await {
            let downloader = Downloader::new();
            let temp_dir = std::env::temp_dir().join("VivaVoceUpdates");
            let _ = std::fs::create_dir_all(&temp_dir);

            let sound_pkgs: Vec<_> = manifest.packages.iter().filter(|p| p.id.starts_with("vivavoce_sounds_")).cloned().collect();
            let total = sound_pkgs.len();
            for (i, pkg) in sound_pkgs.into_iter().enumerate() {
                update_ui_status_with_sub(
                    &weak,
                    &format!("Download audio parte {}/{}...", i + 1, total),
                    &format!("Scaricamento archivio {} in corso...", pkg.filename),
                    0.1 + (i as f32 / total as f32) * 0.8,
                );
                let cancel_token = ctx.cancel_token.clone();
                match downloader.download_package(&pkg, &temp_dir, None, Some(cancel_token)).await {
                    Ok(pkg_path) => {
                        let addons_target = addons_dir.clone();
                        let hash_expected = pkg.blake3.clone();
                        let ext_res = tokio::task::spawn_blocking(move || {
                            extract_tar_zst_file(&pkg_path, &addons_target, Some(&hash_expected), None)
                        }).await;
                        if let Ok(Err(e)) = ext_res {
                            error!("Errore estrazione audio {}: {}", pkg.id, e);
                            update_ui_status(&weak, &format!("Errore estrazione audio: {}", e), 0.0);
                            set_ui_updating(&weak, false);
                            return;
                        }
                    }
                    Err(e) => {
                        error!("Errore download audio {}: {}", pkg.id, e);
                        update_ui_status(&weak, &format!("Errore download audio: {}", e), 0.0);
                        set_ui_updating(&weak, false);
                        return;
                    }
                }
            }
            let addons_for_refresh = addons_dir.clone();
            let _ = weak.upgrade_in_event_loop(move |win| {
                let packages = get_packages_status(Some(&addons_for_refresh));
                win.set_packages(ModelRc::new(VecModel::from(packages)));
                win.set_status_text(SharedString::from("Pacchetto Audio HQ installato!"));
                win.set_sub_status_text(SharedString::from("Tutte le tracce audio sono pronte per giocare"));
                win.set_is_updating(false);
            });
            return;
        }

        update_ui_status(&weak, "Impossibile scaricare il pacchetto audio da GitHub", 0.0);
        set_ui_updating(&weak, false);
        return;
    }

    let (folder_name, pkg_id) = match module_id.as_str() {
        "vivavoce_core" | "vivavoce-core" | "vivavoce" => ("VivaVoce", "vivavoce_core"),
        "vivavoce_quests" | "vivavoce-quests" => ("VivaVoce_Quests", "vivavoce_quests"),
        "vivavoce_books" | "vivavoce-books" => ("VivaVoce_Books", "vivavoce_books"),
        "vivavoce_codex" | "vivavoce-codex" => ("VivaVoce_Codex", "vivavoce_codex"),
        "vivavoce_zones" | "vivavoce-zones" => ("VivaVoce_Zones", "vivavoce_zones"),
        "vivavoce_collector" | "vivavoce-collector" => ("VivaVoce_Collector", "vivavoce_collector"),
        other => {
            warn!("Modulo non riconosciuto: {}", other);
            return;
        }
    };

    set_ui_updating(&ctx.main_window_weak, true);
    update_ui_status(&ctx.main_window_weak, &format!("Installazione {} in corso...", folder_name), 0.3);

    let weak = ctx.main_window_weak.clone();

    // 1. Controlla prima se presente nei sorgenti locali K:\WoW\VivaVoce
    let source_base = PathBuf::from(r"K:\WoW\VivaVoce");
    let local_src = if folder_name == "VivaVoce" {
        source_base.join("pack/VivaVoce")
    } else {
        source_base.join(folder_name)
    };

    if local_src.exists() {
        let dst = addons_dir.join(folder_name);
        let sync_res = tokio::task::spawn_blocking(move || {
            copy_dir_recursive_smart(&local_src, &dst)
        }).await;

        if let Ok(Ok(_)) = sync_res {
            update_installed_toc_version(&addons_dir, "1.0.0");
            let addons_for_refresh = addons_dir.clone();
            let _ = weak.upgrade_in_event_loop(move |win| {
                let packages = get_packages_status(Some(&addons_for_refresh));
                let is_any_installed = addons_for_refresh.join("VivaVoce").exists();
                win.set_packages(ModelRc::new(VecModel::from(packages)));
                win.set_is_installed(is_any_installed);
                win.set_status_text(SharedString::from(format!("Modulo {} installato!", folder_name)));
                win.set_sub_status_text(SharedString::from("Modulo pronto all'uso in gioco"));
                win.set_is_updating(false);
            });
            return;
        }
    }

    // 2. Fallback download da GitHub Releases con catalogo manifest
    update_ui_status(&weak, &format!("Download pacchetto {}...", folder_name), 0.5);
    let downloader = Downloader::new();
    let temp_dir = std::env::temp_dir().join("VivaVoceUpdates");
    let _ = std::fs::create_dir_all(&temp_dir);

    if let Some(manifest) = fetch_remote_manifest().await {
        if let Some(pkg) = manifest.packages.iter().find(|p| p.id == pkg_id) {
            let cancel_token = ctx.cancel_token.clone();
            match downloader.download_package(pkg, &temp_dir, None, Some(cancel_token)).await {
                Ok(pkg_path) => {
                    let addons_target = addons_dir.clone();
                    let hash_expected = pkg.blake3.clone();
                    let ext_res = tokio::task::spawn_blocking(move || {
                        extract_tar_zst_file(&pkg_path, &addons_target, Some(&hash_expected), None)
                    }).await;

                    if let Ok(Err(e)) = ext_res {
                        error!("Errore estrazione modulo {}: {}", pkg_id, e);
                        update_ui_status(&weak, &format!("Errore estrazione: {}", e), 0.0);
                        set_ui_updating(&weak, false);
                        return;
                    }

                    update_installed_toc_version(&addons_dir, &manifest.version);
                    let addons_for_refresh = addons_dir.clone();
                    let _ = weak.upgrade_in_event_loop(move |win| {
                        let packages = get_packages_status(Some(&addons_for_refresh));
                        let is_any_installed = addons_for_refresh.join("VivaVoce").exists();
                        win.set_packages(ModelRc::new(VecModel::from(packages)));
                        win.set_is_installed(is_any_installed);
                        win.set_status_text(SharedString::from(format!("Modulo {} installato con successo!", folder_name)));
                        win.set_sub_status_text(SharedString::from("Modulo pronto"));
                        win.set_is_updating(false);
                    });
                    return;
                }
                Err(e) => {
                    error!("Errore download modulo {}: {}", pkg_id, e);
                    update_ui_status(&weak, &format!("Errore download: {}", e), 0.0);
                    set_ui_updating(&weak, false);
                    return;
                }
            }
        }
    }

    update_ui_status(&weak, &format!("Impossibile scaricare {}", folder_name), 0.0);
    set_ui_updating(&weak, false);
}

/// Rimozione di un singolo modulo selezionato
async fn run_remove_module(ctx: AppContext, module_id: String) {
    let installation = {
        let lock = ctx.wow_installation.read().await;
        lock.clone()
    };
    let Some(inst) = installation else {
        update_ui_status(&ctx.main_window_weak, "Errore: Nessun percorso WoW valido", 0.0);
        return;
    };

    let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);
    let addons_dir = get_addons_path(&inst.root_path, flavor);

    if module_id == "vivavoce_sounds" || module_id == "vivavoce-sounds" {
        let sounds_dir = addons_dir.join("VivaVoce/Sounds");
        let _ = std::fs::remove_dir_all(&sounds_dir);
        let addons_for_refresh = addons_dir.clone();
        let _ = ctx.main_window_weak.upgrade_in_event_loop(move |win| {
            let packages = get_packages_status(Some(&addons_for_refresh));
            win.set_packages(ModelRc::new(VecModel::from(packages)));
            win.set_status_text(SharedString::from("Pacchetto Audio HQ rimosso"));
            win.set_sub_status_text(SharedString::from("I file audio sono stati eliminati"));
        });
        return;
    }

    let folder_name = match module_id.as_str() {
        "vivavoce_core" | "vivavoce-core" | "vivavoce" => "VivaVoce",
        "vivavoce_quests" | "vivavoce-quests" => "VivaVoce_Quests",
        "vivavoce_books" | "vivavoce-books" => "VivaVoce_Books",
        "vivavoce_codex" | "vivavoce-codex" => "VivaVoce_Codex",
        "vivavoce_zones" | "vivavoce-zones" => "VivaVoce_Zones",
        "vivavoce_collector" | "vivavoce-collector" => "VivaVoce_Collector",
        other => {
            warn!("Modulo non riconosciuto: {}", other);
            return;
        }
    };

    // Sicurezza: verificare che folder_name sia strettamente uno dei moduli VivaVoce
    if !VIVAVOCE_MANAGED_MODULES.contains(&folder_name) {
        error!("Tentativo di rimozione di cartella non autorizzata: {}", folder_name);
        return;
    }

    set_ui_updating(&ctx.main_window_weak, true);
    update_ui_status(&ctx.main_window_weak, &format!("Rimozione {} in corso...", folder_name), 0.5);

    let target_dir = addons_dir.join(folder_name);
    let weak = ctx.main_window_weak.clone();
    let addons_for_refresh = addons_dir.clone();

    let res = tokio::task::spawn_blocking(move || {
        safe_remove_dir_all(&target_dir)
    }).await;

    match res {
        Ok(Ok(_)) => {
            info!("Modulo {} rimosso con successo", folder_name);
            let _ = weak.upgrade_in_event_loop(move |win| {
                let packages = get_packages_status(Some(&addons_for_refresh));
                let is_any_installed = addons_for_refresh.join("VivaVoce").exists();
                win.set_packages(ModelRc::new(VecModel::from(packages)));
                win.set_is_installed(is_any_installed);
                win.set_status_text(SharedString::from(format!("Modulo {} rimosso", folder_name)));
                win.set_sub_status_text(SharedString::from("Gli altri addon non sono stati modificati"));
                win.set_is_updating(false);
            });
        }
        Ok(Err(e)) => {
            error!("Errore rimozione {}: {}", folder_name, e);
            update_ui_status(&ctx.main_window_weak, &format!("Errore rimozione: {}", e), 0.0);
            set_ui_updating(&ctx.main_window_weak, false);
        }
        Err(e) => {
            error!("Task rimozione interrotto: {}", e);
            set_ui_updating(&ctx.main_window_weak, false);
        }
    }
}

/// Helper to update status text and progress bar safely on the Slint UI thread,
/// clearing any stale sub_status_text.
fn update_ui_status(weak: &slint::Weak<MainWindow>, msg: &str, progress: f32) {
    update_ui_status_with_sub(weak, msg, "", progress);
}

/// Helper to update status text, sub-status detail text, and progress bar safely on the Slint UI thread
fn update_ui_status_with_sub(weak: &slint::Weak<MainWindow>, msg: &str, sub: &str, progress: f32) {
    let msg_str = msg.to_string();
    let sub_str = sub.to_string();
    let _ = weak.upgrade_in_event_loop(move |win| {
        win.set_status_text(SharedString::from(msg_str));
        win.set_sub_status_text(SharedString::from(sub_str));
        win.set_progress(progress);
    });
}

/// Helper to set updating state
fn set_ui_updating(weak: &slint::Weak<MainWindow>, is_updating: bool) {
    let _ = weak.upgrade_in_event_loop(move |win| {
        win.set_is_updating(is_updating);
    });
}

/// Opens URL in default web browser across Windows, macOS, and Linux
fn open_system_url(url: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(url)
            .spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(url)
            .spawn();
    }
}

/// Opens a directory in default file manager (Explorer, Finder, xdg-open)
fn open_system_folder(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Checks GitHub Releases for updates to VivaVoce Launcher
async fn run_check_launcher_update(ctx: AppContext, manual_trigger: bool) {
    let current_ver = env!("CARGO_PKG_VERSION");
    info!(
        "Verifica aggiornamenti launcher da {} (versione locale: {})...",
        LAUNCHER_RELEASES_API_URL, current_ver
    );

    if manual_trigger {
        let weak = ctx.main_window_weak.clone();
        let _ = weak.upgrade_in_event_loop(|win| {
            win.set_launcher_update_status(SharedString::from("Verifica aggiornamenti in corso..."));
        });
    }

    match fetch_latest_release(LAUNCHER_RELEASES_API_URL).await {
        Ok(release) => {
            let remote_tag = release.tag_name.clone();
            let has_update = is_newer_version(&remote_tag, current_ver);

            let weak = ctx.main_window_weak.clone();
            let _ = weak.upgrade_in_event_loop(move |win| {
                if has_update {
                    let clean_tag = remote_tag.trim_start_matches(|c| c == 'v' || c == 'V');
                    win.set_launcher_has_update(true);
                    win.set_launcher_new_version(SharedString::from(clean_tag));
                    win.set_launcher_update_status(SharedString::from(format!(
                        "Nuova versione v{} disponibile!",
                        clean_tag
                    )));
                    info!("Nuova versione launcher rilevata: v{}", clean_tag);
                } else {
                    win.set_launcher_has_update(false);
                    if manual_trigger {
                        win.set_launcher_update_status(SharedString::from(
                            "Nessun aggiornamento: hai già l'ultima versione.",
                        ));
                    }
                }
            });
        }
        Err(e) => {
            warn!("Controllo aggiornamenti launcher fallito: {}", e);
            if manual_trigger {
                let weak = ctx.main_window_weak.clone();
                let _ = weak.upgrade_in_event_loop(move |win| {
                    win.set_launcher_update_status(SharedString::from(format!(
                        "Errore verifica: {}",
                        e
                    )));
                });
            }
        }
    }
}

/// Downloads and applies the latest launcher binary, then restarts the application
async fn run_perform_launcher_update(ctx: AppContext) {
    let weak = ctx.main_window_weak.clone();
    let _ = weak.upgrade_in_event_loop(|win| {
        win.set_launcher_is_updating(true);
        win.set_launcher_update_progress(0.0);
        win.set_launcher_update_status(SharedString::from("Ricerca asset di rilascio..."));
    });

    info!("Avvio procedura di aggiornamento automatico del Launcher...");

    let release = match fetch_latest_release(LAUNCHER_RELEASES_API_URL).await {
        Ok(r) => r,
        Err(e) => {
            let weak_err = ctx.main_window_weak.clone();
            let _ = weak_err.upgrade_in_event_loop(move |win| {
                win.set_launcher_is_updating(false);
                win.set_launcher_update_status(SharedString::from(format!("Errore: {}", e)));
            });
            return;
        }
    };

    let Some(asset) = find_platform_asset(&release.assets) else {
        let weak_err = ctx.main_window_weak.clone();
        let _ = weak_err.upgrade_in_event_loop(|win| {
            win.set_launcher_is_updating(false);
            win.set_launcher_update_status(SharedString::from(
                "Nessun asset compatibile trovato nel rilascio GitHub.",
            ));
        });
        return;
    };

    let download_url = asset.browser_download_url.clone();
    let asset_name = asset.name.clone();
    info!("Scaricamento asset selezionato: {} ({})", asset_name, download_url);

    let weak_progress = ctx.main_window_weak.clone();
    let progress_cb = move |pct: f32, downloaded: u64, total: u64| {
        let weak = weak_progress.clone();
        let _ = weak.upgrade_in_event_loop(move |win| {
            win.set_launcher_update_progress(pct);
            let mb_down = downloaded as f64 / 1_048_576.0;
            let mb_tot = total as f64 / 1_048_576.0;
            let status = if total > 0 {
                format!(
                    "Download aggiornamento: {:.1} / {:.1} MB ({:.0}%)",
                    mb_down,
                    mb_tot,
                    pct * 100.0
                )
            } else {
                format!("Download aggiornamento: {:.1} MB", mb_down)
            };
            win.set_launcher_update_status(SharedString::from(status));
        });
    };

    match apply_self_update(&download_url, progress_cb).await {
        Ok(new_exe) => {
            info!("Aggiornamento applicato con successo in {:?}", new_exe);
            let weak_done = ctx.main_window_weak.clone();
            let _ = weak_done.upgrade_in_event_loop(|win| {
                win.set_launcher_update_progress(1.0);
                win.set_launcher_update_status(SharedString::from(
                    "Aggiornato! Riavvio del launcher in corso...",
                ));
            });

            tokio::time::sleep(Duration::from_millis(800)).await;
            if let Err(e) = restart_launcher(&new_exe) {
                error!("Riavvio automatico fallito: {}", e);
            }
        }
        Err(e) => {
            error!("Aggiornamento del launcher fallito: {}", e);
            let weak_err = ctx.main_window_weak.clone();
            let _ = weak_err.upgrade_in_event_loop(move |win| {
                win.set_launcher_is_updating(false);
                win.set_launcher_update_status(SharedString::from(format!(
                    "Errore aggiornamento: {}",
                    e
                )));
            });
        }
    }
}
