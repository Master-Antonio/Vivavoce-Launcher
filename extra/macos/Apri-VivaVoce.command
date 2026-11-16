#!/bin/bash
# ==============================================================================
# VivaVoce Launcher - Script di Avvio e Sblocco Sicurezza Gatekeeper per macOS
# ==============================================================================
# Questo script rimuove l'attributo di quarantena di Apple (com.apple.quarantine)
# applicato automaticamente da Safari/Chrome ai file scaricati da GitHub,
# e avvia il Launcher senza che macOS lo blocchi.
# ==============================================================================

DIR="$(cd "$(dirname "$0")" && pwd)"
APP_PATH="$DIR/VivaVoce Launcher.app"

echo "=================================================="
echo "    VivaVoce Launcher - Sblocco macOS Gatekeeper  "
echo "=================================================="

if [ ! -d "$APP_PATH" ]; then
    if [ -d "$DIR/../VivaVoce Launcher.app" ]; then
        APP_PATH="$DIR/../VivaVoce Launcher.app"
    else
        echo "Errore: 'VivaVoce Launcher.app' non trovata nella cartella:"
        echo "$DIR"
        echo ""
        read -p "Premi INVIO per chiudere..."
        exit 1
    fi
fi

echo "[1/3] Rimozione attributi di quarantena macOS (xattr -cr)..."
xattr -cr "$APP_PATH" 2>/dev/null || true

echo "[2/3] Applicazione firma ad-hoc locale (codesign)..."
codesign --force --deep --sign - "$APP_PATH" 2>/dev/null || true

echo "[3/3] Avvio di VivaVoce Launcher..."
open "$APP_PATH"

echo "Avvio completato con successo!"
sleep 1
exit 0
