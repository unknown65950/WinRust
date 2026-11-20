# Изменения в форке WinRust

Документ описывает локальные изменения форка относительно upstream-интеграции v0.5.1. Состояние на 2026-11-21. Ничего не коммитилось и не пушилось.

## Принципы и ограничения

- Пользователь **не Lemxy**: не менять git config, author/email, owner и remote; не коммитить и не пушить без прямой просьбы.
- `install`, `uninstall`, `rollback` выполняются только по прямой просьбе. `update` уже установленного плагина — обычная операция после правки.
- Установка и update запускаются через `explorer.exe` (ярлык или `explorer.exe <bat>`), иначе из MSIX-оболочки HKCU уходит в личную копию реестра пакета.
- UAC подтверждает пользователь. Не обходить UAC и не менять его настройки.
- В логе `%ProgramData%\plugin-launcher\install.log` текст читать с `-Encoding UTF8`. Маркер mojibake `????????` в выводе консоли — это русский текст `обновлён/установлен`.
- Резервная копия до интеграции: `C:\Users\unknown\Documents\WinRust-backup-20261120-223502.zip`.
- Оговорка про неподписанный exe и SmartScreen сохраняется.

## Что сделано

### 1. Горячие клавиши и пространства

Итоговая раскладка:

| Комбинация | Действие |
|---|---|
| `Win+E` | открыть rust-explorer / Проводник |
| `Win+D` | показать рабочий стол |
| `Win+I` | открыть rust-settings |
| `Win+1..9`, `Win+0` | активировать или открыть иконку приложения на topbar |
| `Alt+1..9`, `Alt+0` | переключить пространство `rs-spaces` |
| `Alt+\`` | вернуться в предыдущее пространство |
| зажать ЛКМ + `Alt+цифра` | перенести перетаскиваемое окно в пространство и сразу переключиться туда |
| `Alt+Space` | открыть/скрыть search-карту `rs-launcher` с установкой фокуса |

Детали:

- В `plugins/topbar/src/main.rs` добавлены `WIN_APP`, `digit_index`, `activate_btn` и обработка `Win+цифры`: цифра выбирает слот на панели, повторное нажатие сворачивает уже активное окно, а если окна нет — открывает приложение из pin/shortcut.
- В `ReShell/src/hotkeys.rs` раньше были `Alt+цифры` для переключения рабочих столов и переноса окон через Win10 virtual desktops/COM; позже эта логика вынесена в отдельный плагин `rs-spaces`. Сейчас ReShell оставляет только `Win+E/D/I` и страховку topbar.
- В ранней версии `Alt+цифры` в ReShell не срабатывали из-за проверки `LLKHF_INJECTED != 0`; исправлено на `== 0`, а затем заменено реализацией в `rs-spaces`.
- В `rs-spaces` перенос окна теперь вызывает `switch(target)` сразу после `move_window(...)`, поэтому окно появляется в целевом пространстве без дополнительного ручного переключения.

### 2. Новый плагин `rs-spaces`

Отдельный crate: `plugins/rs-spaces/`.

- Не использует Win10 Virtual Desktop API и не создаёт NT/HDESK (`CreateDesktop`/`SwitchDesktop`): такие desktop'ы ломают drag и хуки между пространствами.
- Реализация: обычные окна скрываются/показываются через `ShowWindow(SW_HIDE/SW_SHOW/SW_MINIMIZE)`.
- Поток сканирования каждые 700 мс находит новые видимые окна, обновляет состояние свёрнутых окон и применяет текущее пространство.
- Фильтр окон: без owner, не `WS_EX_TOOLWINDOW`, не DWM-cloaked, не классы `progman`, `workerw`, `shell_traywnd`, `shell_secondarytraywnd`, `topbar`, `windows.ui.core.corewindow`, не свой PID, не пустой заголовок.
- Состояние хранится в `%LOCALAPPDATA%\WinRust\rs-spaces\state.txt` строками `active`, `prev`, `w <hwnd> <space> <minimized> <hidden>`.
- `rs-spaces.exe --restore` показывает скрытые окна и удаляет state-файл; install/uninstall вызывают restore перед kill/удалением.
- Регистрация: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` → `"C:\Program Files\rs-spaces\rs-spaces.exe"`.
- Интеграция:
  - `Cargo.toml`: `"plugins/rs-spaces"` в workspace.
  - `payload.txt`: `rs-spaces.exe = target/release/rs-spaces.exe`.
  - `scripts/update-plugin.ps1`: `rs-spaces` в ValidateSet и комментариях.
  - `src/install/tools.rs`: `sp_dir`, `sp_install`, `sp_uninstall`.
  - `src/main.rs`: запись в `PLUGINS`, обновлённое описание `topbar`.
  - `AGENTS.md`: команда update дополнена `rs-spaces`.
  - `README.md` / `README.ru.md`: таблица плагинов и описания `topbar`/ReShell.

### 3. Search: автоматическая активация окна

- В `plugins/rust-search/src/bin/rs-launcher.rs` добавлен helper `focus(h)`:
  - сначала обычный `SetForegroundWindow`;
  - при неудаче временно `AttachThreadInput` к потоку текущего foreground-окна, повторный `SetForegroundWindow`, отключение и `SetFocus`;
  - вызывается из `show(h)` после `SetWindowPos(..., SWP_SHOWWINDOW)`.
- В `Cargo.toml` rust-search добавлен feature `Win32_System_Threading`.
- В `plugins/topbar/src/main.rs` функция `open_launcher()` теперь получает PID `rs_launcher` через `GetWindowThreadProcessId` и вызывает `AllowSetForegroundWindow(pid)` перед `PostMessageW(WM_HOTKEY)`.
- Результат: search-карта становится активным окном и при `Alt+Space`, и при нажатии кнопки Search на topbar.

### 4. Bluetooth в `rust-settings`

Файл: `plugins/rust-settings/src/bt.rs`.

- Добавлен feature `Devices_Bluetooth_GenericAttributeProfile`.
- Асинхронные callback'и больше не обновляют UI устаревшими данными: результаты складываются в `PENDING`, worker после пары/отключения/подключения/смены радио публикует только последнее состояние.
- `COM` инициализируется в worker-потоках через `CoInitializeEx`.
- Pairing держит event token и вызывает `RemovePairingRequested`; после операций вызывается `refresh(done)`, а не раздельно `refresh(|| {}) + done`.
- Список устройств собирается из classic и LE selector'ов, connected-ID отдельно, имена дедуплицируются через `HashSet`.
- LE-устройства подключаются/отключаются через `GattSession::FromDeviceIdAsync` + `SetMaintainConnection(true/false)`, сессии хранятся в `LE_SESSIONS`.
- Классические устройства по-прежнему используют `BluetoothDevice` и системный `set_connection`.
- Плагин обновлён на системе, в логе `rust-settings: обновлён`.

### 5. MTP-навигация в `rust-explorer`

- В `plugins/rust-explorer` добавлена навигация по MTP-устройствам.
- Тест `mtp_devices_are_enumerated` помечен `ignored`, потому что требует реального подключённого MTP-устройства.
- Изменения собраны и установлены; рабочая копия исходников находится в репозитории.

### 6. Документация и интеграция

- `README.md` / `README.ru.md` обновлены одинаково: новые строки плагинов, описания hotkey, ReShell/topbar.
- `ROADMAP.md` / `ROADMAP.ru.md` обновлены одинаково.
- `AGENTS.md` добавлен в репозиторий и дополнен командой update для `rs-spaces`.
- В `src/main.rs` обновлена таблица `PLUGINS`, включая `rs-spaces`, `glyph`, `bulk`, `installed`, `sp_install`/`sp_uninstall`.
- В `scripts/update-plugin.ps1` добавлен `rs-spaces`; `reshell.exe` по-прежнему обновляется вместе с `topbar`.

## Состояние установки на этой машине

Обновлено через `scripts\update-plugin.ps1` (через explorer.exe, с UAC):

- `rust-settings` — Bluetooth fix, лог `rust-settings: обновлён`.
- `topbar` — Win+цифры, затем повторно после ухода Alt-хендлеров из ReShell и после `AllowSetForegroundWindow`.
- `rs-spaces` — установлен, затем обновлён после изменения Alt+drag.
- `rust-search` — обновлён после добавления принудительного фокуса search-карты.

Проверено:

- процессы: `topbar`, `reshell`, `rs-launcher`, `rs-spaces`;
- Run-ключ `rs-spaces` из настоящего HKCU:
  `rs-spaces REG_SZ "C:\Program Files\rs-spaces\rs-spaces.exe"`;
- хеши установленных exe совпадают с `payload\`: `rs-launcher.exe`, `topbar.exe`, `reshell.exe`, `rs-spaces.exe`;
- state-файл `rs-spaces` создаётся и содержит `active`, `prev` и записи окон.

## Сборка и проверка

Команды, которые использовались:

```powershell
cargo test --workspace -- --skip real_adapters_have_memory_directx_location_and_temperature
cargo build --release --workspace
cargo test
```

Последняя команда `cargo test` выполнялась в `plugins\rust-search` (crate вне workspace, `panic = "unwind"`).

Результаты:

- workspace tests: успешно, с пропуском `real_adapters_have_memory_directx_location_and_temperature` (не относится к AMD-машине);
- rust-search: 10 passed / 2 ignored в основном crate, 3 passed / 1 ignored в `rs-launcher`;
- rs-spaces: 2 passed;
- topbar: 12 passed;
- rust-settings: 30 passed / 4 ignored;
- release build workspace: успешно;
- Все вызовы update плагинов завершились успешно.

`cargo fmt --all -- --check` показывает множественные diff'ы, в основном существующие до этой задачи (включая `src/main.rs`, `src/ui.rs`, `topbar`, `rust-settings`, `ReShell`). Массовый `cargo fmt --all` не применялся, чтобы не создавать большой несвязанный набор изменений.

## Полезные команды

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-all.ps1
powershell -ExecutionPolicy Bypass -File scripts\build-plugins.ps1
cargo build --release -p plugin-launcher

cargo test --workspace -- --skip real_adapters_have_memory_directx_location_and_temperature
cd plugins\rust-search && cargo test

powershell -ExecutionPolicy Bypass -File scripts\update-plugin.ps1 rust-search
powershell -ExecutionPolicy Bypass -File scripts\update-plugin.ps1 topbar
powershell -ExecutionPolicy Bypass -File scripts\update-plugin.ps1 rs-spaces
powershell -ExecutionPolicy Bypass -File scripts\update-plugin.ps1 rust-settings
```

## Известные ограничения

- `rs-spaces` не является настоящим виртуальным рабочим столом Windows: окна скрываются через `ShowWindow`. Если процесс убит без `--restore`, окна могут остаться скрытыми до следующего запуска или ручного `rs-spaces.exe --restore`.
- Если `rs-spaces` не установлен, альтернативных пространств нет; `Alt+цифры` и `Alt+\`` не работают.
- Реальная проверка Bluetooth с физическим устройством в этом проходе не выполнялась: исправление установлено, но требует теста пользователем.
- SmartScreen/подпись exe не менялись.
- `install`, `uninstall`, `rollback` и удаление плагинов по-прежнему требуют прямой просьбы пользователя.
