# Публикация LChat в «центр приложений» (GNOME Software на Fedora)

## Про «сертификаты» — коротко

В Linux **платных подписей/сертификатов, как на Windows (Authenticode) или macOS
(Apple Developer), НЕ нужно.** Ничего покупать не надо. Для магазина приложений нужны
не сертификаты, а **метаданные**: иконка, `.desktop` и **AppStream metainfo XML**.
Подпись пакетов делает уже сама площадка (Flathub подписывает свой репозиторий OSTree;
Fedora/COPR подписывают RPM своими ключами).

Всё нужное уже готово в этой папке:

| Файл | Назначение |
|------|-----------|
| `icons/hicolor/**/apps/io.github.DevMercenary.LChat.png` + `scalable/…svg` | иконка приложения (тема hicolor) |
| `io.github.DevMercenary.LChat.ico` | иконка для Windows-сборки |
| `io.github.DevMercenary.LChat.desktop` | пункт меню |
| `io.github.DevMercenary.LChat.metainfo.xml` | описание для магазина (проверено `appstreamcli validate`) |
| `io.github.DevMercenary.LChat.yaml` | манифест Flatpak |
| `generate_icon.py` | пересборка иконки |
| `../LICENSE` | лицензия MIT (обязательна для публикации) |

Идентификатор приложения: **`io.github.DevMercenary.LChat`** (reverse-DNS от
`github.com/DevMercenary/LChat` — так требует Flathub).

---

## Что сделать до любой публикации

1. **Сделать репозиторий публичным** (сейчас приватный) — Flathub/COPR берут исходники.
2. **Лицензия.** Добавлен `LICENSE` (MIT). Если хочешь другую — поменяй файл и поле
   `project_license` в metainfo, и `license` в `Cargo.toml`.
3. **Скриншот.** Положи `packaging/screenshots/main.png` (см. там README) и запушь —
   на него ссылается metainfo. Без скриншота Flathub не примет.
4. **Тег версии:** `git tag v0.1.0 && git push --tags`.

---

## Вариант A — Flathub (рекомендуется; появляется в GNOME Software на Fedora)

Это основной путь для стороннего GUI-приложения. Оно попадёт в тот самый «центр
приложений» на всех дистрибутивах с Flatpak.

### 1. Локально собрать и проверить

```bash
flatpak install flathub org.freedesktop.Platform//24.08 org.freedesktop.Sdk//24.08 \
  org.freedesktop.Sdk.Extension.rust-stable//24.08
sudo dnf install flatpak-builder   # если ещё нет

flatpak-builder --user --install --force-clean build-dir \
  packaging/io.github.DevMercenary.LChat.yaml
flatpak run io.github.DevMercenary.LChat
```

Манифест в этом виде собирает crates из сети (`--share=network`) — удобно для теста.

### 2. Offline-источники — уже готовы

Готовый к отправке комплект лежит в **`packaging/flathub/`**:
- `io.github.DevMercenary.LChat.yaml` — манифест для Flathub (offline, git-тег `v0.1.0`
  с пином коммита);
- `cargo-sources.json` — все зависимости из `Cargo.lock` (1059 записей), сгенерированы
  `flatpak-cargo-generator`.

Пересоздать при обновлении версии:
```bash
python3 flatpak-cargo-generator.py Cargo.lock -o packaging/flathub/cargo-sources.json
# затем обновить tag/commit в packaging/flathub/io.github.DevMercenary.LChat.yaml
```

### 3. Отправить в Flathub

1. Форкни `github.com/flathub/flathub`, создай ветку `new-pr`.
2. Скопируй в корень форка **оба** файла из `packaging/flathub/`:
   `io.github.DevMercenary.LChat.yaml` и `cargo-sources.json`.
3. Открой Pull Request → бот соберёт, ревьюер проверит. После мержа создаётся
   отдельный репозиторий и приложение публикуется в Flathub (и появляется в GNOME
   Software на Fedora).

> Перед PR полезно собрать локально (нужен `flatpak-builder`):
> ```bash
> flatpak-builder --user --install --force-clean build-dir \
>   packaging/flathub/io.github.DevMercenary.LChat.yaml
> flatpak run io.github.DevMercenary.LChat
> ```

Полное руководство: https://docs.flathub.org/docs/for-app-authors/submission

> ⚠️ Ограничение в песочнице Flatpak: вставка **скопированных в файловом менеджере
> файлов** (через `wl-paste`/`xclip`) внутри sandbox не работает — этих утилит там нет.
> Картинки из буфера, drag-&-drop и выбор файла через диалог работают штатно.

---

## Вариант B — COPR (RPM для Fedora)

Проще для чисто-Fedora аудитории; пакет ставится как обычный RPM и (если включить репо)
тоже виден в GNOME Software.

1. Нужен `.spec` (собирает `cargo build --release`, ставит бинарник, `.desktop`,
   иконки и metainfo из этой папки).
2. Залогинься на https://copr.fedorainfracloud.org, создай проект, укажи ссылку на
   `.spec` / SRPM или на Git.
3. Пользователи: `sudo dnf copr enable devmercenary/lchat && sudo dnf install lchat`.

(Скажи — сгенерирую готовый `lchat.spec`.)

---

## Вариант C — только локальная интеграция (без магазина)

`./install.sh` уже ставит бинарник, иконку, `.desktop` **и metainfo** в `~/.local/share`.
После этого LChat виден в меню приложений и корректно отображается локально. Это не
«публикация», но для двух своих компов достаточно.

---

## Проверка метаданных (полезно перед PR)

```bash
desktop-file-validate packaging/io.github.DevMercenary.LChat.desktop
appstreamcli validate packaging/io.github.DevMercenary.LChat.metainfo.xml
```
