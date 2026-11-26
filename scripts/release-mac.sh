#!/usr/bin/env bash
# Builds a signed, notarized macOS (Apple silicon) release and the files the
# in-app updater reads, then prints the `gh release create` command.
#
#   scripts/release-mac.sh [release-notes.md]
#
# Run on the release Mac, on a clean main, with the app quit. Needs:
#   - the Developer ID certificate (APPLE_SIGNING_IDENTITY to override)
#   - notarytool keychain profile "mydb-notary" (NOTARY_PROFILE to override)
#   - the updater key from scripts/updater-keygen.sh
#
# Output in ~/workspace/release-<version>/ (OUT to override):
#   MyDB-Studio-<v>-macOS-arm64.zip             download for people
#   MyDB-Studio-<v>-macOS-arm64.app.tar.gz      what the in-app updater installs
#   MyDB-Studio-<v>-macOS-arm64.app.tar.gz.sig  its signature
#   latest.json                                 what the app checks
set -euo pipefail

IDENTITY="${APPLE_SIGNING_IDENTITY:-Developer ID Application: CHUNG HSIEN WU (QCB75M55MY)}"
NOTARY_PROFILE="${NOTARY_PROFILE:-mydb-notary}"
KEY="${UPDATER_KEY:-$HOME/.tauri/mydb-studio-updater.key}"
SERVICE="mydb-studio-updater-key"
REPO="jackiewu/mydb-studio"
NOTES_FILE="${1:-}"

cd "$(dirname "$0")/.."
VERSION="$(node -p "require('./src-tauri/tauri.conf.json').version")"
TAG="v$VERSION"
OUT="${OUT:-$HOME/workspace/release-$VERSION}"
NAME="MyDB-Studio-$VERSION-macOS-arm64"
APP="target/release/bundle/macos/MyDB Studio.app"

fail() { echo "✗ $*" >&2; exit 1; }

# --- checks before spending time on a build ---------------------------------
[[ "$(uname -m)" == arm64 ]] || fail "只支援 Apple silicon（arm64）。"
[[ "$(git rev-parse --abbrev-ref HEAD)" == main ]] || fail "請在 main 分支上發版（IAP 等本機分支不能發布）。"
[[ -z "$(git status --porcelain)" ]] || fail "工作目錄有未提交的變更。"
[[ -z "${MYDB_FLAVOR:-}" ]] || fail "MYDB_FLAVOR=${MYDB_FLAVOR}：本機版本不能發布。"
grep -q 'None => "",' src-tauri/src/update.rs || fail "src-tauri/src/update.rs 的 FLAVOR 不是空的：這是本機版本，不能發布。"
grep -q "^version = \"$VERSION\"" src-tauri/Cargo.toml || fail "src-tauri/Cargo.toml 的版本和 tauri.conf.json（${VERSION}）不一樣。"
grep -q REPLACE_WITH src-tauri/tauri.conf.json && fail "tauri.conf.json 還沒填更新公鑰。"
[[ -f "$KEY" ]] || fail "找不到更新簽章私鑰 ${KEY}（先跑 scripts/updater-keygen.sh，或從備份放回）。"
[[ "$(cat "$KEY.pub" 2>/dev/null)" == "$(node -p "require('./src-tauri/tauri.conf.json').plugins.updater.pubkey")" ]] \
  || fail "$KEY.pub 和 tauri.conf.json 的公鑰不一樣。"
PW="$(security find-generic-password -s "$SERVICE" -a updater -w)" || fail "鑰匙圈裡沒有「${SERVICE}」密碼。"
[[ -z "$NOTES_FILE" || -f "$NOTES_FILE" ]] || fail "找不到 $NOTES_FILE"
pgrep -fq "MyDB Studio.app/Contents/MacOS" && fail "請先結束 MyDB Studio。"
mkdir -p "$OUT"
[[ -z "$(ls -A "$OUT")" ]] || fail "$OUT 不是空的，請先移走裡面的檔案。"

echo "== 建置 ${TAG}（$(git rev-parse --short HEAD)）"
npm ci
APPLE_SIGNING_IDENTITY="$IDENTITY" npm run tauri build -- --bundles app
codesign --verify --deep --strict "$APP"

echo "== 公證"
ditto -c -k --keepParent "$APP" "$OUT/notarize-upload.zip"
xcrun notarytool submit "$OUT/notarize-upload.zip" --keychain-profile "$NOTARY_PROFILE" --wait | tee "$OUT/notarize.log"
grep -q "status: Accepted" "$OUT/notarize.log" || fail "公證沒有通過，看 $OUT/notarize.log"
mv "$OUT/notarize-upload.zip" ~/.Trash/ 2>/dev/null || true
xcrun stapler staple "$APP"
spctl -a -vv "$APP"

echo "== 打包"
ditto -c -k --keepParent "$APP" "$OUT/$NAME.zip"
# No AppleDouble (._*) files or extended attributes: they'd land inside the
# installed bundle and break its code signature.
COPYFILE_DISABLE=1 tar --no-mac-metadata -czf "$OUT/$NAME.app.tar.gz" -C "$(dirname "$APP")" "$(basename "$APP")"

echo "== 簽章（更新用）"
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$PW" npx tauri signer sign \
  -f "$KEY" --app-version "$VERSION" "$OUT/$NAME.app.tar.gz"

NOTES="MyDB Studio $VERSION"
[[ -n "$NOTES_FILE" ]] && NOTES="$(cat "$NOTES_FILE")"
SIG="$(cat "$OUT/$NAME.app.tar.gz.sig")" NOTES="$NOTES" VERSION="$VERSION" \
URL="https://github.com/$REPO/releases/download/$TAG/$NAME.app.tar.gz" \
node -e '
  const e = process.env;
  const json = {
    version: e.VERSION,
    notes: e.NOTES,
    pub_date: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
    platforms: { "darwin-aarch64": { signature: e.SIG, url: e.URL } },
  };
  process.stdout.write(JSON.stringify(json, null, 2) + "\n");
' > "$OUT/latest.json"

echo
echo "== 完成：$OUT"
ls -l "$OUT"
shasum -a 256 "$OUT/$NAME.zip" "$OUT/$NAME.app.tar.gz"
echo
echo "發布（latest.json 一定要附上，app 從最新 release 讀它）："
NOTES_ARG="--notes \"MyDB Studio $VERSION\""
[[ -n "$NOTES_FILE" ]] && NOTES_ARG="--notes-file \"$NOTES_FILE\""
echo "gh release create $TAG --repo $REPO --target $(git rev-parse HEAD) --title \"MyDB Studio $VERSION\" $NOTES_ARG \\"
echo "  \"$OUT/$NAME.zip\" \"$OUT/$NAME.app.tar.gz\" \"$OUT/$NAME.app.tar.gz.sig\" \"$OUT/latest.json\""
