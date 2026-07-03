#!/usr/bin/env bash
# Установщик LChat для Linux (Fedora и др.): сборка + установка в профиль пользователя.
# Права root не нужны — всё ставится в ~/.local.
set -euo pipefail

cd "$(dirname "$0")"

APPID="io.github.DevMercenary.LChat"
BIN_DIR="$HOME/.local/bin"
DATA="$HOME/.local/share"
APP_DIR="$DATA/applications"
ICONS="$DATA/icons/hicolor"
BIN="$BIN_DIR/lchat"
DESKTOP="$APP_DIR/$APPID.desktop"

echo "==> Сборка (release)…"
cargo build --release

echo "==> Установка бинарника в $BIN"
mkdir -p "$BIN_DIR" "$APP_DIR"
install -m 755 target/release/lchat "$BIN"

echo "==> Иконки приложения"
for s in 16 32 48 64 128 256 512; do
  src="packaging/icons/hicolor/${s}x${s}/apps/$APPID.png"
  [ -f "$src" ] && install -Dm644 "$src" "$ICONS/${s}x${s}/apps/$APPID.png"
done
[ -f "packaging/icons/hicolor/scalable/apps/$APPID.svg" ] && \
  install -Dm644 "packaging/icons/hicolor/scalable/apps/$APPID.svg" \
    "$ICONS/scalable/apps/$APPID.svg"

echo "==> Метаданные для «центра приложений» (AppStream)"
install -Dm644 "packaging/$APPID.metainfo.xml" \
  "$DATA/metainfo/$APPID.metainfo.xml"

echo "==> Ярлык в меню приложений: $DESKTOP"
cat > "$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=LChat
GenericName=Local Network Chat
Comment=Локальный P2P-чат по локальной сети
Exec=$BIN
Icon=$APPID
Terminal=false
Categories=Network;Chat;InstantMessaging;
Keywords=chat;lan;p2p;чат;
StartupNotify=true
EOF

# Обновляем базы .desktop и иконок, если утилиты есть.
command -v update-desktop-database >/dev/null 2>&1 && \
  update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && \
  gtk-update-icon-cache -f -t "$ICONS" >/dev/null 2>&1 || true

echo
echo "Готово. Запуск: lchat  (или из меню приложений «LChat»)."
echo

# Подсказки по окружению.
if [ -z "${PATH##*$HOME/.local/bin*}" ]; then :; else
  echo "ВНИМАНИЕ: $HOME/.local/bin не в PATH — добавь в ~/.bashrc:"
  echo '  export PATH="$HOME/.local/bin:$PATH"'
  echo
fi

command -v wl-paste >/dev/null 2>&1 || command -v xclip >/dev/null 2>&1 || {
  echo "Совет: для вставки СКОПИРОВАННЫХ ФАЙЛОВ из файлового менеджера поставь:"
  echo "  Wayland: sudo dnf install wl-clipboard"
  echo "  X11:     sudo dnf install xclip"
  echo "(картинки из буфера работают и без них)"
  echo
}

command -v ffmpeg >/dev/null 2>&1 || \
  echo "Совет: для миниатюр видео поставь ffmpeg:  sudo dnf install ffmpeg"

cat <<'EOF'

Файрвол (для связи с другим компом), выполнить один раз:
  sudo firewall-cmd --add-port=9009/tcp --permanent
  sudo firewall-cmd --add-port=9010/udp --permanent
  sudo firewall-cmd --reload
EOF
