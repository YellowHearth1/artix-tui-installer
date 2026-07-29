
# 🧭 Guía de Arquitectura y Colaboradores

> Un mapa del proyecto para cualquier persona que quiera corregir o añadir algo,
> incluso si Rust es nuevo para ti. Cada cambio común está vinculado a su archivo.

## Flujo de datos en 30 segundos

```
main.rs (terminal, bucle de eventos)
   └─ event.rs (enrutamiento de teclas; any_modal_open — para que Esc cierre el modal, no la pantalla)
       └─ screens/*.rs (15 pantallas: draw() renderiza, handle_key() reacciona)
           └─ app.rs (App — todo el estado; InstallConfig — elecciones del usuario; valores predeterminados)
               └─ system/install/ (build_plan: elecciones → pasos de Acción ordenados)
                   └─ system/runner.rs (ejecuta pasos, transmite el log a la pantalla 14)
```

## Mapa de archivos

| Archivo | Responsabilidad | Cambio típico |
|---|---|---|
| `src/main.rs` | configuración de la terminal, bucle principal | rara vez se toca |
| `src/app.rs` | estado de la app, `InstallConfig`, predeterminados, el enum `Screen` | una nueva opción de usuario → un nuevo campo aquí |
| `src/event.rs` | enrutamiento global de teclas, `any_modal_open()` | nuevo modal → registrar su bandera aquí o Esc saldrá de la pantalla |
| `src/i18n.rs` + `i18n/*.toml` | todo el texto orientado al usuario, uk/en | **cada** clave va en AMBOS tomls (se verifica la paridad) |
| `src/theme.rs` | colores/estilos | — |
| `src/screens/*.rs` | un archivo por pantalla (idioma, disco, wifi, opciones, resumen…) | el comportamiento de esa pantalla |
| `src/screens/wifi.rs` | Wi-Fi: nmcli, fallback de inicio de NetworkManager, lógica de reintento | regla: Enter nunca es una operación nula silenciosa |
| `src/system/install/mod.rs` | `build_plan` — el núcleo: se lee como un índice (25 pasos, una línea cada uno), con el detalle en ocho funciones `plan_*` (ver abajo) | un nuevo paso de instalación |
| `src/system/install/helpers.rs` | constructores de Acción (`act`, `chroot`, `write_target_file`…), LUKS/rootflags | — |
| `src/system/install/scripts.rs` | TODOS los scripts/servicios/dotfiles/assets embebidos | la edición del texto de un script ocurre aquí, y solo aquí |
| `src/system/install/packages.rs` | DE/GPU/kernel → listas de paquetes | añadir un paquete predeterminado |
| `src/system/install/mirrors.rs` | verifica la salud de **cada** mirror (Artix + Arch + Chaotic) antes de instalar: los activos primero (más rápidos), los caídos comentados | — |
| `src/system/disk.rs` | parseo de lsblk, plan de particionamiento | — |
| `src/system/runner.rs` | ejecución del plan, transmisión de logs, `capture()` | — |
| `src/rollback.rs` | rollback de btrfs (selector de snapshots) | — |
| `src/assets/` | configs de waybar/wofi/fastfetch; `pinnacle/` contiene la config del compositor como archivos planos | Config de Pinnacle: solo edita el archivo bajo `assets/pinnacle/` |
| `iso-profile/` | perfil de ISO live para `buildiso` (paquetes, servicios dinit, overlay); pero el perfil que `buildiso` lee realmente es un `profile.yaml` separado (fuera de este repo), no `Packages-Live`/`live-overlay/` aquí | un servicio de ISO live → añadirlo a `live-session.services:` en `profile.yaml` |

## Estructura de `build_plan`

Es el archivo más grande, por lo que vale la pena conocer su forma. `build_plan` no *contiene* la lógica de instalación, sino que la *enumera*:

```rust
pub fn build_plan(app: &App) -> Vec<Action> {
    // 0)  herramientas del host
    // 1)  disco: particionar, formatear, montar
    // 2)  basestrap: base + paquetes elegidos
    // 3)  fstab (+ discos extra)         → plan_fstab()
    // ...
    // 9)  cuentas                      → plan_accounts()
    // 9b) marcadores GTK + sesión D-Bus → plan_session_env()
    // 9c) initramfs + archivos clave LUKS → plan_initramfs_luks()
    // 10) cargador de arranque (bootloader) → plan_bootloader()
    // 11) firewall                      → plan_firewall()
    // 12) servicios dinit + AUR          → plan_services()
}
```

Cada `plan_*` es una **función pura**: lee `InstallConfig`, añade pasos al plan y no hace nada más. Esto es lo que hace que una instalación sea testeable sin tocar un disco:

```rust
let t = plan_text(&build_plan(&app));
assert!(t.contains("groupadd -f log"));
```

**El orden de los pasos importa** (fstab antes del bootloader, cuentas antes de los servicios) y el compilador no lo verificará; no cambies el orden de las llamadas sin una razón.

## Tipos, no strings

`InstallConfig` no almacena las elecciones del usuario como strings. `boot_mode` es un `BootMode`, no `"uefi"`; `bootloader` es un `Bootloader`; `gpu` es un `Vec<GpuDriver>`, no un CSV. Esto no es por estilo:

- añade una variante a un `enum` y el compilador te **obliga** a manejarla en cada `match`, incluyendo el selector en pantalla.
- el conocimiento sobre las dependencias reside **en el tipo**, no disperso en archivos: `SeatProvider::user_launcher()` sabe que elogind va con `userspawn` y seatd con `turnstiled`.

No añadas nuevos campos de string donde el conjunto de valores sea finito.

## Cómo realizar un cambio típico

**Añadir un paquete predeterminado** $\rightarrow$ `src/system/install/packages.rs`, `base_packages` (o un conjunto de DE dentro de él). Verifica que el paquete exista: `pacman -Ss nombre`.

**Añadir texto/traducción** $\rightarrow$ la misma clave en `i18n/uk.toml` Y `i18n/en.toml`; úsala mediante `t(app.lang, "seccion.clave")`. Ver verificación de paridad más abajo.

**Cambiar un script embebido** (rollback, mirrors, la guía de Secure Boot) $\rightarrow$ `src/system/install/scripts.rs`. Los scripts son POSIX sh: verifica con `dash -n`.

**Añadir una pantalla** $\rightarrow$ un nuevo `src/screens/archivo.rs` (copia la más sencilla como plantilla), una variante en `enum Screen` (`app.rs`), una fila en la tabla `step()` (`screens/mod.rs`); esa fila lo es todo: dibujo, teclas, tick, pista del pie de página. El match no tiene un "catch-all", por lo que una pantalla no registrada es un error de compilación, no un vacío silencioso. Modales: no olvides `any_modal_open()` en `event.rs`.

**Añadir un bootloader** $\rightarrow$ `ORDER` en `src/screens/options.rs`, una rama en `match c.bootloader` en `install/mod.rs`, una pista de i18n, README.

**Comportamiento de Wi-Fi** $\rightarrow$ `src/screens/wifi.rs`; el demonio NetworkManager en la ISO live se habilita a través de la lista `live-session.services:` en `profile.yaml` (el archivo que `buildiso` lee realmente), NO un enlace simbólico en `iso-profile/live-overlay/` de este repo; los archivos colocados allí no llegan a la ISO construida.

## Construcción y verificaciones

Todo lo que hace la CI también se ejecuta localmente y debería hacerse antes de hacer commit:

```sh
cd installer
cargo fmt --check                          # un solo estilo
cargo build --release                      # rustc >= 1.90
cargo clippy --release -- -D warnings      # lo que el compilador deja pasar
cargo test --release                       # pruebas de regresión (ver abajo)
```

**Las pruebas son errores que ya sucedieron.** Cada `#[test]` en `system/install/mod.rs` fija un fallo que llegó a un usuario real: la memoria USB que terminó formateada; el grupo `log` sin el cual los logs eran ilegibles; `useradd` antes que `groupadd`; la verificación de AUR que mentía sobre los paquetes `-git`. El plan son datos puros, por lo que una instalación puede inspeccionarse sin tocar un disco:

```rust
let t = plan_text(&build_plan(&app));
assert!(t.contains("groupadd -f log"));
```

¿Encontraste un bug? **Escribe la prueba primero**, luego arréglalo; de lo contrario, el próximo refactor lo traerá de vuelta.

**Tres niveles de pruebas, cada uno capturando algo que los otros no pueden:**

| Dónde | Qué verifica | Un bug que capturó |
|---|---|---|
| `system/install/mod.rs` | el **plan de instalación** — datos puros, no requiere disco | la memoria USB se formateó; `useradd` se ejecutó antes que `groupadd` |
| `screens/mod.rs` | **renderizado**, vía `TestBackend` (dibuja en memoria) | un panic en `draw()`; un cursor más allá del final de una lista; una clave i18n cruda en pantalla |
| `event.rs` | **teclas** — `handle_global` llamado directamente | `q` cerró el instalador desde un campo de contraseña |

`TestBackend` es el backend integrado de ratatui que renderiza en un búfer de memoria en lugar de una terminal, por lo que cualquier pantalla puede dibujarse en una prueba unitaria:

```rust
let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
term.draw(|f| draw(f, &mut app, f.area())).unwrap();
let text = term.backend().to_string();   // toda la pantalla como texto
```

Por qué esto importa específicamente aquí: el instalador se ejecuta en una **consola física desde una ISO live**. Un panic en `draw()` no es un stack trace en un log, es una máquina muerta a mitad de la instalación, sin forma de volver. Por eso, cada pantalla se verifica en tres tamaños (80x24 siendo el mínimo prometido) y en ambos idiomas.

```sh
# paridad de traducción (CI ejecuta lo mismo):
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

**Probar la TUI sin hardware:** el instalador funciona perfectamente en QEMU (UEFI vía OVMF).

**Wi-Fi en una VM sin adaptador — un solo comando.** En la ISO live (como root):

```sh
wifi-test
```

(Se incluye en la ISO live como `/usr/bin/wifi-test`. Fuera de la ISO, ejecuta `sh scripts/wifi-test.sh` desde la raíz del repo).

Carga `mac80211_hwsim` (dos radios virtuales), ejecuta hostapd en una transmitiendo **ArtixTest** / **testtest123**, y deja la otra para el instalador. Luego navega por la pantalla de Wi-Fi normalmente. Vale la pena probar una contraseña **incorrecta** también (debe permanecer en la pantalla con un error) y Enter en una lista vacía (debe re-escanear, nunca quedarse en silencio).

Alternativamente, inicia la ISO desde una memoria USB en una laptop; la pantalla de Wi-Fi aparece a los ~20 segundos, sin necesidad de instalar.

## Estilo

- Los comentarios explican el *porqué*, no el *qué*; las decisiones importantes llevan un bloque arriba del código.
- `rustfmt --edition 2021` antes de hacer commit.
- El shell dentro de `format!` usa `@@PLACEHOLDER@@` + `.replace()`, nunca `{{`.
- Commits: un tema por commit; el mensaje indica el efecto visible para el usuario.
