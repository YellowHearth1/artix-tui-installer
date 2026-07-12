# 🧭 Архітектура та посібник контриб'ютора

> Цей файл — мапа проєкту для тих, хто хоче щось виправити чи додати,
> навіть якщо Rust для вас новий. Кожна типова зміна тут розписана до файлу.

## Потік даних за 30 секунд

```
main.rs (термінал, цикл подій)
   └─ event.rs (роутинг клавіш; any_modal_open — щоб Esc закривав модалку, а не екран)
       └─ screens/*.rs (15 екранів: draw() малює, handle_key() реагує)
           └─ app.rs (App — увесь стан; InstallConfig — вибір користувача; дефолти)
               └─ system/install/ (build_plan: вибір → список Action-кроків)
                   └─ system/runner.rs (виконує кроки, стрімить лог на екран 14)
```

## Мапа файлів

| Файл | За що відповідає | Типові зміни |
|---|---|---|
| `src/main.rs` | запуск терміналу, головний цикл | рідко чіпається |
| `src/app.rs` | стан застосунку, `InstallConfig`, дефолти, перелік `Screen` | новий пункт вибору → нове поле тут |
| `src/event.rs` | глобальний роутинг клавіш, `any_modal_open()` | нова модалка → додай її прапорець сюди, інакше Esc вибиватиме з екрана |
| `src/i18n.rs` + `i18n/*.toml` | усі тексти, укр/англ | **кожен** ключ додається в ОБИДВА toml (parity перевіряється) |
| `src/theme.rs` | кольори/стилі | — |
| `src/screens/*.rs` | по файлу на екран (мова, диск, wifi, options, summary…) | поведінка конкретного екрана |
| `src/screens/wifi.rs` | Wi-Fi: nmcli, страховка запуску NetworkManager, retry-логіка | правило: Enter ніколи не «мовчить» |
| `src/system/install/mod.rs` | `build_plan` — серце: 40+ нумерованих кроків установки | новий крок установки |
| `src/system/install/helpers.rs` | конструктори Action (`act`, `chroot`, `write_target_file`…), LUKS/rootflags | — |
| `src/system/install/scripts.rs` | УСІ вбудовані скрипти/сервіси/дотфайли/асети | правити текст скрипта — тут, і лише тут |
| `src/system/install/packages.rs` | DE/GPU/ядро → списки пакетів | додати пакет за замовчуванням |
| `src/system/install/mirrors.rs` | ранжування дзеркал + таблиця країн за таймзоною | — |
| `src/system/disk.rs` | lsblk-розбір, план розмітки | — |
| `src/system/runner.rs` | виконання плану, стрімінг логу, `capture()` | — |
| `src/rollback.rs` | btrfs-відкат (меню знімків) | — |
| `src/assets/` | waybar/wofi/fastfetch конфіги, тарбол конфігу Pinnacle | конфіг Pinnacle: розпакуй `pinnacle.tar.gz`, зміни, запакуй назад |
| `iso-profile/` | профіль live-ISO для `buildiso` (пакети, dinit-сервіси, overlay) | сервіс на live-ISO → симлінк у `live-overlay/etc/dinit.d/boot.d/` |

## Як зробити типову зміну

**Додати пакет за замовчуванням** → `src/system/install/packages.rs`, функція
`base_packages` (або DE-набір у ній). Перевір, що пакет існує: `pacman -Ss назва`.

**Додати текст/переклад** → однаковий ключ у `i18n/uk.toml` І `i18n/en.toml`,
використання: `t(app.lang, "секція.ключ")`. Перевірка parity — команда нижче.

**Змінити вбудований скрипт** (відкат, дзеркала, Secure Boot-інструкція) →
`src/system/install/scripts.rs`. Скрипти — POSIX sh: перевір `dash -n`.

**Додати екран** → новий `src/screens/файл.rs` (скопіюй найпростіший за
зразок), варіант у `enum Screen` (`app.rs`), гілки в `event.rs` та роутері
draw. Модалки — не забудь `any_modal_open()`.

**Додати завантажувач** → `ORDER` у `src/screens/options.rs`, гілка в
`match c.bootloader` у `install/mod.rs`, i18n-підказка, README.

**Wi-Fi поведінка** → `src/screens/wifi.rs`; демон на ISO вмикається
симлінком `iso-profile/live-overlay/etc/dinit.d/boot.d/NetworkManager`.

## Збірка й перевірки

```sh
cd installer && cargo build --release        # rustc ≥ 1.90
# parity перекладів:
python3 - <<'EOF'
import tomllib
def f(d,p=""):
    s=set()
    for k,v in d.items():
        s|=f(v,p+k+".") if isinstance(v,dict) else {p+k}
    return s
a=f(tomllib.load(open("i18n/uk.toml","rb"))); b=f(tomllib.load(open("i18n/en.toml","rb")))
print("OK" if a==b else a^b)
EOF
```

**Тестування TUI без заліза:** інсталятор чудово ганяється в QEMU (UEFI —
через OVMF).

**Wi-Fi у віртуалці без адаптера — однією командою.** У живому ISO (від root):

```sh
wifi-test
```

(Скрипт лежить на живому ISO як `/usr/bin/wifi-test`. Поза ISO —
`sh scripts/wifi-test.sh` з кореня репозиторію.)

Скрипт вантажить `mac80211_hwsim` (два віртуальні радіо), піднімає на одному
hostapd із мережею **ArtixTest** / паролем **testtest123**, а друге лишає
інсталятору. Далі проходь Wi-Fi-екран як звичайно. Варто перевірити й
**неправильний** пароль (має лишити на екрані з помилкою) та Enter на
порожньому списку (має пересканувати, а не мовчати).

Альтернатива — завантажити ISO з флешки на ноутбуці: до екрана Wi-Fi йти
~20 секунд, установка не потрібна.

## Стиль

- Коментарі пояснюють «чому», а не «що»; великі рішення — блоком над кодом.
- `rustfmt --edition 2021` перед комітом.
- Shell у `format!` — через `@@PLACEHOLDER@@` + `.replace()`, не через `{{`.
- Коміти: одна тема — один коміт; повідомлення описує наслідок для користувача.
