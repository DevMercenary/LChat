#!/usr/bin/env bash
# Установщик LChat для Linux (Fedora и др.): сборка + установка в профиль пользователя.
# Права root не нужны — всё ставится в ~/.local.
set -euo pipefail

cd "$(dirname "$0")"

BIN_DIR="$HOME/.local/bin"
APP_DIR="$HOME/.local/share/applications"
BIN="$BIN_DIR/lchat"
DESKTOP="$APP_DIR/lchat.desktop"

echo "==> Сборка (release)…"
cargo build --release

echo "==> Установка бинарника в $BIN"
mkdir -p "$BIN_DIR" "$APP_DIR"
install -m 755 target/release/lchat "$BIN"

echo "==> Ярлык в меню приложений: $DESKTOP"
cat > "$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=LChat
Comment=Локальный P2P-чат по локальной сети
Exec=$BIN
Terminal=false
Categories=Network;Chat;
StartupNotify=true
EOF

# Обновляем базу .desktop, если утилита есть.
command -v update-desktop-database >/dev/null 2>&1 && \
  update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true

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
