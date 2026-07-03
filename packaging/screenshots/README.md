# Скриншоты для «центра приложений»

AppStream/Flathub требует хотя бы один скриншот. Положи сюда `main.png`
(окно LChat, PNG, ширина ≈ 1200 px), он уже прописан в metainfo по адресу:

```
https://raw.githubusercontent.com/DevMercenary/LChat/main/packaging/screenshots/main.png
```

Как снять на своём рабочем столе:
- GNOME: запусти LChat, нажми `Print Screen` → «Снимок окна», сохрани сюда как `main.png`.
- Или установи grim: `sudo dnf install grim slurp`, затем `grim -g "$(slurp)" packaging/screenshots/main.png`.

Можно добавить несколько скриншотов — просто допиши их в `<screenshots>` в
`packaging/io.github.DevMercenary.LChat.metainfo.xml`.
