# VivaVoce Launcher (Native Rust & Slint)

Launcher e gestore nativo ad altissime prestazioni per l'AddOn di doppiaggio italiano **VivaVoce** (World of Warcraft: Forever / Classic).

Sviluppato da zero in **Rust**, sfrutta **Slint** con backend hardware Skia/Winit per un rendering fluido a 120 FPS in finestra frameless dark/gold in pieno stile World of Warcraft, con un consumo di RAM inferiore a 25 MB e avvio istantaneo.

---

## ⚡ Caratteristiche Principali

1. **Auto-Discovery di World of Warcraft**:
   - Rileva automaticamente l'installazione di World of Warcraft dal registro di sistema di Windows (`winreg`), da `%ProgramData%\Battle.net\Agent\product.db` e dai percorsi standard.
   - Priorità intelligente ai flavor: `_forever_` (WoW: Forever), `_classic_era_`, `_classic_`, `_retail_`.
   - Supporto a selettore cartella nativo (`rfd`) per percorsi personalizzati.

2. **Delta Scanner Parallelo (Blake3 + Rayon)**:
   - Verifica di integrità ultra-veloce su tutti i 27.420 file audio e script Lua tramite hashing Blake3 memory-mapped in parallelo su tutti i core CPU.
   - Calcola il delta esatto tra manifest remoto e file locali in millisecondi.

3. **Streaming Downloader & Extractor Zero-RAM**:
   - Download asincrono streaming via `reqwest` (con TLS nativo `rustls`).
   - Decompressione in streaming da chunk compressi `.tar.zst` direttamente verso la cartella `Interface/AddOns` senza mai accumulare gigabyte in RAM.
   - Sincronizzazione locale istantanea ad alta velocità quando rileva la build locale in `K:\WoW\VivaVoce`.

4. **Disinstallazione Sicura e Pulita**:
   - Rimuove selettivamente solo i 6 moduli VivaVoce (`VivaVoce`, `VivaVoce_Quests`, `VivaVoce_Books`, `VivaVoce_Codex`, `VivaVoce_Zones`, `VivaVoce_Collector`).
   - Non tocca mai altri AddOn terzi dell'utente.
   - Opzione per ripulire le `SavedVariables` e i backup.

5. **Bug Reporter Audio Integrato (SavedVariables)**:
   - Analizza la cartella `WTF/Account/<ACCOUNT>/SavedVariables/VivaVoce.lua` per estrarre la cronologia degli ultimi dialoghi riprodotti in gioco.
   - Precompila automaticamente file audio `.ogg`, Quest ID, Creature ID, nome NPC e testo a schermo.
   - Consente di esportare un report Markdown strutturato, copiare la segnalazione o aprire direttamente una mail a `vivavoce.project@gmail.com` o una issue su GitHub.

6. **Interfaccia Grafica Slint (WoW Dark / Gold)**:
   - Finestra frameless (`no-frame: true`) con angoli arrotondati e cornici runiche.
   - Barra titolo personalizzata con supporto al trascinamento nativo del sistema operativo (`winit::drag_window`).
   - Schede: Dashboard, Pacchetti Audio, Segnalazione Bug, Impostazioni.
   - Barra di avanzamento fluida con visualizzazione percentuale, byte, velocità in MB/s ed ETA.

---

## 🏗️ Architettura del Progetto

```
K:\WoW\VivaVoceLauncher/
├── Cargo.toml                  # Slint 1.18, Tokio, Reqwest, Blake3, Rayon, Zstd, Tar, Winreg
├── build.rs                    # Compilatore Slint per i file dell'interfaccia
├── ui/
│   ├── launcher.slint          # MainWindow con sidebar, tab view e reactive state
│   ├── theme.slint             # Token design WoW: palette dark/gold, pergamena, gradienti
│   └── components/
│       ├── titlebar.slint      # Barra titolo frameless con drag nativo, chiudi e minimizza
│       ├── progress_bar.slint  # Barra di progresso fluida con byte/s ed ETA
│       ├── status_badge.slint  # Badge visivo di stato (Verde, Oro, Rosso, Blu)
│       ├── package_card.slint  # Schede dei 6 moduli VivaVoce con Blake3
│       ├── bug_card.slint      # Modulo segnalazione con lista dialoghi in-game
│       └── buttons.slint       # Bottoni primari oro, secondari e danger
└── src/
    ├── main.rs                 # Entrypoint Tokio, bridge Slint-Rust e orchestrazione eventi
    ├── wow/
    │   ├── mod.rs
    │   ├── discovery.rs        # Scanner Registro Windows, product.db e flavors WoW
    │   └── saved_variables.rs  # Parser Lua zero-copy per cronologia dialoghi in-game
    ├── engine/
    │   ├── mod.rs
    │   ├── manifest.rs         # Modelli dati RemoteManifest e LocalState
    │   ├── scanner.rs          # Scansione Blake3 + Rayon parallela O(1)/O(N)
    │   ├── downloader.rs       # Streaming async downloader con stima ETA
    │   ├── extractor.rs        # Decompressione streaming tar.zst costante < 30 MB RAM
    │   └── uninstaller.rs      # Disinstallazione sicura con whitelist protetta
    └── bug_reporter/
        ├── mod.rs
        ├── collector.rs        # Generatore report Markdown fortemente tipizzato
        └── submitter.rs        # Esportatore su disco, mailto e link GitHub
```

---

## 🧪 Test Unitari

Tutti i 28 test unitari passano al 100%:

```bash
cargo test
```

Verifica statica e linter:
```bash
cargo clippy -- -D warnings
```

---

## 🚀 Compilazione Release

Per creare il file eseguibile ottimizzato (`vivavoce-launcher.exe`):

```bash
cargo build --release
```

L'eseguibile standalone si troverà in `target/release/vivavoce-launcher.exe`.
