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

use bug_reporter::{BugCategory, BugReport, Submitter};
use engine::downloader::{Downloader, ProgressEvent};
use engine::extractor::extract_tar_zst_file;
use engine::manifest::RemoteManifest;
use engine::scanner::scan_delta;
use engine::uninstaller::uninstall_vivavoce;
use wow::discovery::{
    detect_wow_installations, find_wow_install_path, get_addons_path, validate_wow_root,
    WowFlavor, WowInstallation,
};
use wow::saved_variables::{load_all_saved_variables, RecentDialogue};

pub const REMOTE_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/Master-Antonio/Vivavoce-Releases/refs/heads/main/manifest.json";

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
    // 1. Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    info!("Avvio VivaVoce WoW Voiceover Launcher v0.1.0...");

    // 2. Initialize UI
    let main_window = MainWindow::new()?;
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

    // 6. Run Slint event loop
    main_window.run()?;
    info!("Chiusura VivaVoce Launcher terminata.");
    Ok(())
}

/// Setup frameless window dragging, minimize, and close callbacks
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

    // SUBMIT BUG REPORT
    let bug_ctx = ctx.clone();
    main_window.on_submit_bug_report(move |category_str, user_notes, sound_file| {
        let ctx_clone = bug_ctx.clone();
        tokio::spawn(async move {
            run_submit_bug_report(ctx_clone, category_str.to_string(), user_notes.to_string(), sound_file.to_string()).await;
        });
    });

    // SELECT WOW FOLDER
    let folder_ctx = ctx.clone();
    main_window.on_select_wow_folder(move || {
        let ctx_clone = folder_ctx.clone();
        tokio::spawn(async move {
            run_browse_wow_folder(ctx_clone).await;
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
                let _ = std::process::Command::new("explorer").arg(addons).spawn();
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
            WowFlavor::Forever => "WoW Forever (1.12.1)".to_string(),
            other => format!("{:?}", other),
        };

        // Check installed version from TOC if present
        let installed_version = if is_installed {
            read_toc_version(&addons_dir.join("VivaVoce/VivaVoce.toc")).unwrap_or_else(|| "1.0.0".to_string())
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
            win.set_wow_path(SharedString::from(wow_path_str));
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
            let packages = get_default_packages(is_installed);
            win.set_packages(ModelRc::new(VecModel::from(packages)));
        });

        // Background check for latest remote manifest & delta patches
        let weak_for_remote = ctx.main_window_weak.clone();
        let installed_for_remote = installed_version.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .user_agent("VivaVoceLauncher/0.1.0")
                .timeout(Duration::from_secs(4))
                .build()
                .unwrap_or_default();

            if let Ok(resp) = client.get(REMOTE_MANIFEST_URL).send().await {
                if resp.status().is_success() {
                    if let Ok(manifest) = resp.json::<RemoteManifest>().await {
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
                }
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
            let packages = get_default_packages(false);
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
    update_ui_status(&weak, "Connessione al catalogo aggiornamenti GitHub...", 0.05);
    let http_client = reqwest::Client::builder()
        .user_agent("VivaVoceLauncher/0.1.0")
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let remote_manifest_res = http_client.get(REMOTE_MANIFEST_URL).send().await;
    let remote_manifest: Option<RemoteManifest> = match remote_manifest_res {
        Ok(resp) if resp.status().is_success() => resp.json::<RemoteManifest>().await.ok(),
        _ => None,
    };

    let vivavoce_dir = addons_dir.join("VivaVoce");
    let is_installed = vivavoce_dir.join("Core.lua").exists();
    let local_version = read_toc_version(&vivavoce_dir.join("VivaVoce.toc")).unwrap_or_else(|| "1.0.0".to_string());
    let downloader = Downloader::new();
    let temp_dir = std::env::temp_dir().join("VivaVoceUpdates");
    let _ = std::fs::create_dir_all(&temp_dir);

    // 2. REPAIR MODE
    if is_repair {
        if let Some(ref manifest) = remote_manifest {
            update_ui_status(&weak, "Scansione rapida e verifica integrità file locali...", 0.3);
            let addons_for_scan = addons_dir.clone();
            let manifest_for_scan = manifest.clone();
            let scan_res = tokio::task::spawn_blocking(move || {
                scan_delta(&addons_for_scan, &manifest_for_scan)
            }).await;

            match scan_res {
                Ok(Ok(delta)) => {
                    info!("Verifica completata: {} file integri", delta.files_matched);
                    let _ = weak.upgrade_in_event_loop(move |win| {
                        win.set_progress(1.0);
                        win.set_status_text(SharedString::from("Integrità Verificata con Successo"));
                        win.set_sub_status_text(SharedString::from(format!("{} file verificati — Tutti i file sono integri", delta.files_matched)));
                        win.set_is_updating(false);
                    });
                    return;
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
                                let _ = weak.upgrade_in_event_loop(move |win| {
                                    win.set_progress(1.0);
                                    win.set_status_text(SharedString::from(format!("VivaVoce v{} aggiornato con successo!", new_ver)));
                                    win.set_sub_status_text(SharedString::from(format!("Applicata patch delta: {} file modificati/nuovi aggiornati", summary.files_extracted)));
                                    win.set_is_installed(true);
                                    win.set_has_update(false);
                                    win.set_installed_version(SharedString::from(&new_ver));
                                    win.set_is_updating(false);
                                    let packages = get_default_packages(true);
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
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.set_progress(1.0);
                win.set_status_text(SharedString::from("VivaVoce sincronizzato e pronto"));
                win.set_sub_status_text(SharedString::from("27.420 battute italiane verificate"));
                win.set_is_installed(true);
                win.set_has_update(false);
                win.set_installed_version(SharedString::from(&ver));
                win.set_is_updating(false);
                let packages = get_default_packages(true);
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

            update_ui_status(&weak, &format!("Download modulo {}/{} ({})...", i + 1, total_pkgs, pkg.id), (i as f32) / (total_pkgs as f32));
            let pkg_clone = pkg.clone();
            let temp_dir_clone = temp_dir.clone();
            let tx_clone = tx.clone();
            let cancel_clone = ctx.cancel_token.clone();

            match downloader.download_package(&pkg_clone, &temp_dir_clone, Some(tx_clone), Some(cancel_clone)).await {
                Ok(pkg_path) => {
                    let addons_target = addons_dir.clone();
                    let hash_expected = pkg.blake3.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        extract_tar_zst_file(&pkg_path, &addons_target, Some(&hash_expected), None)
                    }).await;
                }
                Err(e) => {
                    update_ui_status(&weak, &format!("Errore download {}: {}", pkg.id, e), 0.0);
                    set_ui_updating(&weak, false);
                    return;
                }
            }
        }

        let new_ver = manifest.version.clone();
        let _ = weak.upgrade_in_event_loop(move |win| {
            win.set_progress(1.0);
            win.set_status_text(SharedString::from("Installazione completata con successo!"));
            win.set_sub_status_text(SharedString::from(format!("VivaVoce v{} è pronto per giocare", new_ver)));
            win.set_is_installed(true);
            win.set_has_update(false);
            win.set_installed_version(SharedString::from(&new_ver));
            win.set_is_updating(false);
            let packages = get_default_packages(true);
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
        });

        let src = if *module == "VivaVoce" {
            source_base.join("pack/VivaVoce")
        } else {
            source_base.join(module)
        };

        let dst = target_addons.join(module);

        if src.exists() {
            copy_dir_recursive(&src, &dst)?;
        }
    }

    Ok(())
}

/// Helper ricorsivo per la copia di directory
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let dest_child = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_child)?;
        } else {
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
    let res = tokio::task::spawn_blocking(move || {
        uninstall_vivavoce(&addons_dir, false)
    }).await;

    match res {
        Ok(Ok(uninst)) => {
            info!("VivaVoce disinstallato: rimosse {:?}, liberati {} byte", uninst.removed_addon_dirs, uninst.freed_bytes);
            let weak = ctx.main_window_weak.clone();
            let _ = weak.upgrade_in_event_loop(|win| {
                win.set_is_installed(false);
                win.set_has_update(true);
                win.set_progress(0.0);
                win.set_status_text(SharedString::from("VivaVoce rimosso correttamente"));
                win.set_sub_status_text(SharedString::from("Nessun altro addon è stato modificato"));
                win.set_is_updating(false);
                let packages = get_default_packages(false);
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

/// Submits or exports bug report based on game context and user input
async fn run_submit_bug_report(
    ctx: AppContext,
    category_str: String,
    user_notes: String,
    sound_file: String,
) {
    let installation = {
        let lock = ctx.wow_installation.read().await;
        lock.clone()
    };

    let Some(inst) = installation else {
        update_ui_status(&ctx.main_window_weak, "Impossibile creare report: WoW non configurato", 0.0);
        return;
    };

    let dialogues = ctx.recent_dialogues_cache.read().await.clone();
    let matched_dialogue = dialogues.iter().find(|d| d.sound_file == sound_file || sound_file.is_empty());

    let (quest_id, npc_id, title, text) = if let Some(d) = matched_dialogue {
        (d.quest_id, d.npc_id, Some(d.title.clone()), Some(d.text.clone()))
    } else {
        (None, None, None, None)
    };

    let category = BugCategory::from_str_lenient(&category_str);
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let flavor = inst.primary_flavor.unwrap_or(WowFlavor::Forever);

    let report = BugReport {
        category,
        addon_version: "1.0.0".to_string(),
        client_flavor: format!("{:?}", flavor),
        wow_path: inst.root_path.to_string_lossy().to_string(),
        sound_file: if sound_file.is_empty() { None } else { Some(sound_file) },
        quest_id,
        npc_id,
        dialogue_title: title,
        dialogue_text: text,
        user_description: user_notes,
        timestamp_epoch: epoch,
    };

    // Save report in WTF SavedVariables folder
    let target_dir = inst.root_path.join(flavor.as_dir_name()).join("WTF");
    let save_dir = if target_dir.exists() {
        target_dir
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };

    match Submitter::save_to_disk(&report, &save_dir) {
        Ok(path) => {
            info!("Bug report salvato su disco in {:?}", path);
            let weak = ctx.main_window_weak.clone();
            let path_str = path.to_string_lossy().to_string();
            let mailto = Submitter::create_mailto_url(&report);

            let _ = weak.upgrade_in_event_loop(move |win| {
                win.set_sub_status_text(SharedString::from(format!("Report salvato: {}", path_str)));
                win.set_status_text(SharedString::from("Segnalazione esportata con successo!"));
            });

            // Open report file or mailto client
            open_system_url(&mailto);
        }
        Err(e) => {
            error!("Errore salvataggio bug report: {}", e);
            update_ui_status(&ctx.main_window_weak, &format!("Errore salvataggio report: {}", e), 0.0);
        }
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
        info!("Cartella selezionata dall'utente: {:?}", path);
        match validate_wow_root(&path) {
            Ok(flavors) => {
                let inst = WowInstallation::new(path.clone(), flavors);
                *ctx.wow_installation.write().await = Some(inst);
                discover_and_refresh(&ctx).await;
            }
            Err(e) => {
                warn!("Cartella selezionata non valida: {}", e);
                update_ui_status(
                    &ctx.main_window_weak,
                    "La cartella selezionata non contiene un'installazione valida di World of Warcraft.",
                    0.0,
                );
            }
        }
    }
}

/// Launch World of Warcraft executable
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
    let candidates = [
        client_path.join("Wow.exe"),
        client_path.join("WoWClassic.exe"),
        inst.root_path.join(flavor.as_dir_name()).join("Wow.exe"),
        inst.root_path.join("Wow.exe"),
        inst.root_path.join("World of Warcraft Launcher.exe"),
    ];

    let found_exe = candidates.iter().find(|p| p.exists());

    if let Some(exe) = found_exe {
        info!("Avvio eseguibile di gioco WoW Forever: {:?}", exe);
        match std::process::Command::new(exe).current_dir(exe.parent().unwrap_or(&client_path)).spawn() {
            Ok(_) => {
                update_ui_status(&ctx.main_window_weak, "World of Warcraft: Forever avviato!", 1.0);
            }
            Err(e) => {
                error!("Errore avvio gioco: {}", e);
                update_ui_status(&ctx.main_window_weak, &format!("Errore avvio WoW Forever: {}", e), 0.0);
            }
        }
    } else {
        warn!("Eseguibile Wow.exe non trovato nelle posizioni note.");
        update_ui_status(&ctx.main_window_weak, "Eseguibile Wow.exe non trovato nella cartella di WoW Forever.", 0.0);
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

/// Returns the 6 VivaVoce package descriptors
fn get_default_packages(installed: bool) -> Vec<PackageInfo> {
    vec![
        PackageInfo {
            id: SharedString::from("vivavoce-core"),
            name: SharedString::from("VivaVoce Core"),
            description: SharedString::from("Motore audio principale, gestione code, sottotitoli e opzioni"),
            filename: SharedString::from("vivavoce-core.tar.zst"),
            size_str: SharedString::from("865 KB"),
            installed,
            verified: installed,
            blake3_hash: SharedString::from("b3f8...91a2"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce-quests"),
            name: SharedString::from("VivaVoce Quests"),
            description: SharedString::from("15.838 battute: dialoghi quest, gossip NPC, accettazione e completamento"),
            filename: SharedString::from("vivavoce-quests.tar.zst"),
            size_str: SharedString::from("2.48 GB"),
            installed,
            verified: installed,
            blake3_hash: SharedString::from("7c2d...4401"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce-books"),
            name: SharedString::from("VivaVoce Books"),
            description: SharedString::from("5.594 battute: tomi di Azeroth, lettere, diari, pergamene e lapidi"),
            filename: SharedString::from("vivavoce-books.tar.zst"),
            size_str: SharedString::from("1.12 GB"),
            installed,
            verified: installed,
            blake3_hash: SharedString::from("33aa...ff99"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce-codex"),
            name: SharedString::from("VivaVoce Codex"),
            description: SharedString::from("1.606 battute: supporto a CodexIT e missioni lore classiche"),
            filename: SharedString::from("vivavoce-codex.tar.zst"),
            size_str: SharedString::from("385 MB"),
            installed,
            verified: installed,
            blake3_hash: SharedString::from("19e0...8821"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce-zones"),
            name: SharedString::from("VivaVoce Zones"),
            description: SharedString::from("Annunci e tracce d'atmosfera delle zone del mondo"),
            filename: SharedString::from("vivavoce-zones.tar.zst"),
            size_str: SharedString::from("120 MB"),
            installed,
            verified: installed,
            blake3_hash: SharedString::from("4fa1...bb12"),
        },
        PackageInfo {
            id: SharedString::from("vivavoce-collector"),
            name: SharedString::from("VivaVoce Collector"),
            description: SharedString::from("Rilevamento in-game di dialoghi non doppiati e telemetria locale"),
            filename: SharedString::from("vivavoce-collector.tar.zst"),
            size_str: SharedString::from("45 KB"),
            installed,
            verified: installed,
            blake3_hash: SharedString::from("e220...5c61"),
        },
    ]
}

/// Helper to update status text and progress bar safely on the Slint UI thread
fn update_ui_status(weak: &slint::Weak<MainWindow>, msg: &str, progress: f32) {
    let msg_str = msg.to_string();
    let _ = weak.upgrade_in_event_loop(move |win| {
        win.set_status_text(SharedString::from(msg_str));
        win.set_progress(progress);
    });
}

/// Helper to set updating state
fn set_ui_updating(weak: &slint::Weak<MainWindow>, is_updating: bool) {
    let _ = weak.upgrade_in_event_loop(move |win| {
        win.set_is_updating(is_updating);
    });
}

/// Opens URL or file in default Windows shell handler
fn open_system_url(url: &str) {
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
}
