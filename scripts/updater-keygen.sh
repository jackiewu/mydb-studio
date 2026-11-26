#!/usr/bin/env bash
# Makes the key pair that signs in-app updates. Run once, on the Mac that
# builds releases.
#
#   private key  ~/.tauri/mydb-studio-updater.key   (never commit or upload it)
#   its password macOS Keychain, service "mydb-studio-updater-key"
#   public key   printed at the end; it goes in src-tauri/tauri.conf.json
#                (plugins.updater.pubkey)
#
# Back up the key file and the password (e.g. in a password manager). Without
# them no installed copy can be updated in-app again; each would need a manual
# download of a release made with a new key.
set -euo pipefail

KEY="${UPDATER_KEY:-$HOME/.tauri/mydb-studio-updater.key}"
SERVICE="mydb-studio-updater-key"

cd "$(git rev-parse --show-toplevel)"

if [[ -e "$KEY" ]]; then
  echo "已經有金鑰：${KEY}（不覆蓋）。公鑰："
  cat "$KEY.pub"
  echo
  exit 0
fi

mkdir -p "$(dirname "$KEY")"
chmod 700 "$(dirname "$KEY")"

# A random password, kept only in the Keychain.
PW="$(openssl rand -base64 32)"
security add-generic-password -U -s "$SERVICE" -a updater -l "MyDB Studio updater key password" -w "$PW"

npx tauri signer generate --ci -p "$PW" -w "$KEY"
chmod 600 "$KEY"

echo
echo "私鑰：${KEY}（請備份，不要放進 git）"
echo "密碼：存在鑰匙圈「${SERVICE}」（請一起備份）"
echo "公鑰（放進 src-tauri/tauri.conf.json → plugins.updater.pubkey）："
cat "$KEY.pub"
echo
