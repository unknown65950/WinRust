# WinRust (plugin-launcher)

Установщик `plugin-launcher.exe` ставит и удаляет плагины, которые заменяют части Windows. Для пользователя: `README.md`, планы: `ROADMAP.md`.

## ⚠ Окружение: MSIX и HKCU

Если оболочка, из которой работает агент, упакована в MSIX, всё, что запущено из неё (даже через UAC), пишет HKCU в личную копию реестра пакета, которую настоящий сеанс не видит (так терялись Run-ключи, `ReplaceExplorer`, привязка папок). HKLM пишется по-настоящему.
- Установщик (`install`, `update`) запускай **только через explorer.exe** (ярлык или `explorer.exe <bat>`). `update-plugin.ps1` уже делает это сам.
- `reg query HKCU\...` из такой оболочки показывает копию пакета, а не правду. Настоящие значения смотри батником, запущенным через `explorer.exe <bat>`.

Если оболочка не в MSIX, это ограничение не действует, но запуск через explorer.exe остаётся безопасным правилом.

## Где что лежит

| Путь | Что |
|---|---|
| `src/main.rs` | таблица `PLUGINS` и общее состояние |
| `src/install/` | что каждый плагин меняет в системе и как откатывается (файл на плагин; `mod.rs`: общие хелперы, payload, `install_plugin`/`rollback`) |
| `src/cli.rs`, `src/ui.rs` | командная строка и окно |
| `common/` | `winrust-common`: `t!`/`tf!`/`en!` (язык) и `mft` (чтение MFT) для всех программ |
| `plugins/<имя>/`, `ReShell/` | исходники плагинов (это и есть оригиналы: копий на рабочем столе больше нет, правь здесь) |
| `payload.txt` | единственный список вшиваемых exe: из него читают `build.rs` и `scripts\build-plugins.ps1` |

Один cargo workspace, все exe в `target\release`. `plugins/rust-search` и `plugins/chrome-rs` вне workspace (им нужен `panic = "unwind"`), у них свой `target\`.

## Сборка и проверка

- Полная сборка: `powershell -ExecutionPolicy Bypass -File scripts\build-all.ps1` (плагины → `payload\` → установщик).
- Только плагины: `scripts\build-plugins.ps1`. Только установщик: `cargo build --release -p plugin-launcher` (нужен заполненный `payload\`).
- Тесты: `cargo test --workspace` и `cargo test` в `plugins\rust-search`. У самого установщика тестового exe нет (`test = false`: манифест требует админа).
- В юнит-тестах тексты всегда русские: в коде с тестами пиши `en!()`, а не `english()`.

## Обновить установленный плагин после правки (без пересборки установщика)

```powershell
powershell -ExecutionPolicy Bypass -File scripts\update-plugin.ps1 <rust-search|rtaskmgr-win32|topbar|rust-explorer|wiztree|photoview|rust-settings|rs-spaces|debloat>
```

Скрипт пересобирает плагины, обновляет `payload\` и запускает `plugin-launcher update <плагин>` через explorer.exe. Появится окно UAC: **«Да» нажимает пользователь**. Не обходи UAC и не меняй его настройки.

После запуска:
1. Скрипт печатает хвост `%ProgramData%\plugin-launcher\install.log` (UTF-8, читай с `-Encoding UTF8`). Успех: `<плагин>: updated` (`обновлён`), предупреждения после `|`. Ошибка: `ERROR ...` (`ОШИБКА ...`).
2. Проверь, что процесс запущен (`Get-Process topbar,reshell` и т. п.).
3. Нет `updated`/`обновлён` или есть ошибка: покажи пользователю текст ошибки, сам ничего не удаляй и не чини реестр.

## Правила

- `update` только для уже установленного плагина. `install <имя> --yes`, `uninstall`, `rollback` **только по прямой просьбе пользователя** (rust-search отключает поиск Windows, topbar прячет панель задач, rust-explorer отключает Проводник).
- Код установки (`src/install/`) ни разу не прогонялся целиком на живой системе: тестируй осторожно, по одному плагину.
- Новый плагин: crate в `plugins/` + `members` в `Cargo.toml`, строка в `payload.txt`, файл в `src/install/`, запись в `PLUGINS`. Перед изменением системы сохраняй исходные значения, чтобы uninstall их вернул.
- Не коммитить и не пушить без прямой просьбы. Не добавлять в репозиторий данные аккаунта, author/email или удалённого владельца.
- Не коммитить `payload\*.exe`, `target\`, логи (они в `.gitignore`).

## Документация

- `README.md` (англ.) и `README.ru.md` (рус.) править одинаково. Так же `ROADMAP.md` / `ROADMAP.ru.md`.
- Бенчмарк в README обновлять только после реального нового замера (`scripts\bench.ps1`), цифры не придумывать.
- Оговорку про неподписанный exe и SmartScreen не убирать, пока нет подписи.
- Настройки репозитория и страницы проекта менять только по просьбе владельца.
