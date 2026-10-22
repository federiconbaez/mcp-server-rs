# mcp-server-rs

Servidor [Model Context Protocol](https://modelcontextprotocol.io) escrito desde cero en Rust, **sin el SDK oficial**: JSON-RPC 2.0 propio, transportes stdio y Streamable HTTP, y una herramienta de ejemplo que se ejecuta dentro de un sandbox seccomp.

*A from-scratch Rust MCP server (no official SDK). English version below.*

---

## Español

### Qué es

Un servidor MCP pequeño y auditable, pensado para entender el protocolo a bajo nivel y para servir de base a herramientas propias. Todo el protocolo (framing, validación JSON-RPC, ciclo de vida, sesiones HTTP, validación de cabeceras) está implementado a mano; las únicas dependencias son `serde_json`, `tiny_http`, `getrandom`, `base64` y, en Linux, `seccompiler` y `libc`.

### Revisiones del protocolo

El servidor es *dual-era*:

| Revisión | Modo | Estado |
|---|---|---|
| `2026-07-28` (actual) | sin estado, `_meta` por request, `server/discover` | parcial (ver limitaciones) |
| `2025-11-25` | handshake `initialize` | implementado |
| `2025-06-18` | handshake `initialize` | implementado |
| `2025-03-26` | handshake `initialize` + batches JSON-RPC | implementado |
| `2024-11-05` (HTTP+SSE) | — | no soportado |

Un request con `_meta["io.modelcontextprotocol/protocolVersion"]` se atiende en modo moderno; un `initialize` abre una sesión legacy (por proceso en stdio, por `Mcp-Session-Id` en HTTP).

### Qué está implementado

- **JSON-RPC 2.0**: requests, notificaciones, respuestas del cliente (se ignoran), códigos `-32700`, `-32600`, `-32601`, `-32602`, `-32603`. Ids string o entero; `null` y no enteros se rechazan (regla de MCP). Batches sólo con la versión negociada `2025-03-26` (las revisiones posteriores los eliminaron); batch vacío → un único `-32600`; batch sólo de notificaciones → sin respuesta.
- **Ciclo de vida legacy**: `initialize` (negociación de versión), `notifications/initialized`, `ping`, rechazo de requests antes de inicializar y de un segundo `initialize`.
- **Modo moderno**: `server/discover`, `UnsupportedProtocolVersion` (`-32022`), `resultType: "complete"`, `serverInfo` en `_meta`, `ttlMs`/`cacheScope` en resultados cacheables, `ping` eliminado, recurso inexistente = `-32602`.
- **Funciones**: `tools/list`, `tools/call` (errores de validación de argumentos como `isError: true`), `resources/list`, `resources/read`, `resources/templates/list`, `prompts/list`, `prompts/get`.
- **stdio**: un mensaje por línea, líneas vacías ignoradas, cierre limpio al recibir EOF.
- **Streamable HTTP** (`POST /mcp`):
  - validación de `Origin` (403; loopback siempre permitido, más `--allow-origin`), `Content-Type` (415), `Accept` (406), tamaño de body (413);
  - sesiones legacy con `Mcp-Session-Id` aleatorio de 128 bits, 400 sin sesión, 404 sesión desconocida, `DELETE` para cerrarla, validación de `MCP-Protocol-Version`;
  - modo moderno sin sesión, con validación cabecera↔body de `MCP-Protocol-Version`, `Mcp-Method` y `Mcp-Name` (incluida la codificación `=?base64?…?=`) → `400` + `-32020`; método desconocido → `404` + `-32601`;
  - `GET` → `405` (no se ofrece stream SSE independiente).
- **Herramientas**: `echo` y `calc` (sólo Linux).

### Herramienta en sandbox: `calc`

`calc` evalúa expresiones aritméticas, pero **nunca dentro del proceso del servidor**. El servidor se re-ejecuta como worker (`mcp-server-rs __sandbox-worker calc`) con entorno vacío y `cwd=/`. El worker:

1. baja sus `rlimit` (CPU 2 s, memoria 256 MiB, archivos 0 bytes, 0 procesos, 4 descriptores);
2. activa `PR_SET_NO_NEW_PRIVS` e instala un filtro seccomp-BPF de lista blanca: `read`, `write`, `exit`, `exit_group`, `brk`, `mmap`, `munmap`, `mremap`, `madvise`, `futex`, `sigaltstack`, `rt_sigreturn`, `rt_sigprocmask`. Cualquier otra syscall mata el proceso con `SIGSYS`;
3. lee la expresión de stdin, responde una línea JSON y termina.

El padre aplica un timeout de 3 s y limita la salida a 64 KiB. Los tests demuestran que `open(2)` y la creación de procesos dentro del worker terminan en `SIGSYS`.

### Uso

```sh
cargo build --release
# stdio (por defecto)
./target/release/mcp-server-rs
# HTTP en loopback
./target/release/mcp-server-rs http --bind 127.0.0.1:8808
```

Ejemplo de configuración stdio para un cliente MCP:

```json
{ "mcpServers": { "rs": { "command": "/ruta/a/mcp-server-rs", "args": ["stdio"] } } }
```

### Tests

`cargo test` ejecuta tests unitarios y de integración escritos a partir del texto de la especificación: casos borde de JSON-RPC y ciclo de vida sobre stdio (proceso real), el transporte HTTP sobre sockets reales y el sandbox. **No** es la suite de conformidad oficial de MCP; son tests propios.

### Limitaciones (honestas)

- Las respuestas HTTP son siempre `application/json`: no se emiten streams SSE, ni notificaciones de progreso/log, ni `Last-Event-ID`.
- No hay requests servidor→cliente (sampling, elicitation, roots), ni MRTR (`input_required`), ni `subscriptions/listen`, ni tasks, logging, completion o progress.
- `notifications/cancelled` se acepta pero no tiene efecto: los requests se procesan de forma síncrona.
- Sin autorización (OAuth). Usalo sólo en loopback o detrás de un proxy que autentique.
- No se validan cabeceras `Mcp-Param-*` (ninguna herramienta usa `x-mcp-header`); en modo moderno `clientCapabilities` no es obligatorio.
- El validador de JSON Schema cubre sólo el subconjunto que usan las herramientas incluidas (`type`, `properties`, `required`, `additionalProperties: false`, `maxLength`).
- Sin paginación (cualquier `cursor` es inválido); no se detectan ids repetidos; las sesiones HTTP no expiran (máximo 1024) y no hay TLS.
- En stdio no hay límite de longitud de línea.
- El sandbox es sólo seccomp + rlimits: no usa namespaces ni Landlock, y no restringe argumentos de `mmap` (p. ej. `PROT_EXEC`). Sólo Linux (x86_64, aarch64, riscv64). En otros sistemas `calc` no se registra.

### Licencia

Apache-2.0. Ver [LICENSE](LICENSE).

---

## English

### What it is

A small, auditable MCP server built to understand the protocol at a low level and to host custom tools. The whole protocol layer (framing, JSON-RPC validation, lifecycle, HTTP sessions, header validation) is hand-written; the only dependencies are `serde_json`, `tiny_http`, `getrandom`, `base64` and, on Linux, `seccompiler` and `libc`.

### Protocol revisions

The server is *dual-era*:

| Revision | Mode | Status |
|---|---|---|
| `2026-07-28` (current) | stateless, per-request `_meta`, `server/discover` | partial (see limitations) |
| `2025-11-25` | `initialize` handshake | implemented |
| `2025-06-18` | `initialize` handshake | implemented |
| `2025-03-26` | `initialize` handshake + JSON-RPC batches | implemented |
| `2024-11-05` (HTTP+SSE) | — | not supported |

A request carrying `_meta["io.modelcontextprotocol/protocolVersion"]` is served in modern mode; an `initialize` opens a legacy session (per process on stdio, per `Mcp-Session-Id` on HTTP).

### What is implemented

- **JSON-RPC 2.0**: requests, notifications, client responses (ignored), error codes `-32700`, `-32600`, `-32601`, `-32602`, `-32603`. String or integer ids; `null` and non-integer ids are rejected (MCP rule). Batches only when `2025-03-26` was negotiated (later revisions removed them); empty batch → single `-32600`; notification-only batch → no reply.
- **Legacy lifecycle**: `initialize` (version negotiation), `notifications/initialized`, `ping`, rejection of requests before initialization and of a second `initialize`.
- **Modern mode**: `server/discover`, `UnsupportedProtocolVersion` (`-32022`), `resultType: "complete"`, `serverInfo` in `_meta`, `ttlMs`/`cacheScope` on cacheable results, `ping` removed, missing resource = `-32602`.
- **Features**: `tools/list`, `tools/call` (argument validation failures reported as `isError: true`), `resources/list`, `resources/read`, `resources/templates/list`, `prompts/list`, `prompts/get`.
- **stdio**: one message per line, blank lines ignored, clean exit on EOF.
- **Streamable HTTP** (`POST /mcp`):
  - `Origin` validation (403; loopback always allowed, plus `--allow-origin`), `Content-Type` (415), `Accept` (406), body size (413);
  - legacy sessions with a random 128-bit `Mcp-Session-Id`, 400 without session, 404 for unknown session, `DELETE` to terminate, `MCP-Protocol-Version` validation;
  - sessionless modern mode with header↔body validation of `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` (including `=?base64?…?=` encoding) → `400` + `-32020`; unknown method → `404` + `-32601`;
  - `GET` → `405` (no standalone SSE stream is offered).
- **Tools**: `echo` and `calc` (Linux only).

### Sandboxed tool: `calc`

`calc` evaluates arithmetic expressions, but **never inside the server process**. The server re-executes itself as a worker (`mcp-server-rs __sandbox-worker calc`) with an empty environment and `cwd=/`. The worker:

1. lowers its `rlimit`s (CPU 2 s, 256 MiB address space, 0-byte files, 0 processes, 4 descriptors);
2. sets `PR_SET_NO_NEW_PRIVS` and installs an allow-list seccomp-BPF filter: `read`, `write`, `exit`, `exit_group`, `brk`, `mmap`, `munmap`, `mremap`, `madvise`, `futex`, `sigaltstack`, `rt_sigreturn`, `rt_sigprocmask`. Any other syscall kills the process with `SIGSYS`;
3. reads the expression from stdin, writes one JSON line and exits.

The parent enforces a 3 s timeout and caps output at 64 KiB. Tests prove that `open(2)` and process creation inside the worker end in `SIGSYS`.

### Usage

```sh
cargo build --release
# stdio (default)
./target/release/mcp-server-rs
# HTTP on loopback
./target/release/mcp-server-rs http --bind 127.0.0.1:8808
```

stdio configuration example for an MCP client:

```json
{ "mcpServers": { "rs": { "command": "/path/to/mcp-server-rs", "args": ["stdio"] } } }
```

### Tests

`cargo test` runs unit and integration tests written from the specification text: JSON-RPC edge cases and lifecycle over stdio (real child process), the HTTP transport over real sockets, and the sandbox. This is **not** the official MCP conformance suite; these are the project's own tests.

### Limitations (honest)

- HTTP responses are always `application/json`: no SSE streams, no progress/log notifications, no `Last-Event-ID`.
- No server→client requests (sampling, elicitation, roots), no MRTR (`input_required`), no `subscriptions/listen`, no tasks, logging, completion or progress.
- `notifications/cancelled` is accepted but has no effect: requests are processed synchronously.
- No authorization (OAuth). Run it on loopback only, or behind an authenticating proxy.
- `Mcp-Param-*` headers are not validated (no tool uses `x-mcp-header`); in modern mode `clientCapabilities` is not required.
- The JSON Schema validator only covers the subset used by the bundled tools (`type`, `properties`, `required`, `additionalProperties: false`, `maxLength`).
- No pagination (any `cursor` is invalid); duplicate request ids are not detected; HTTP sessions never expire (max 1024) and there is no TLS.
- No line-length limit on stdio.
- The sandbox is seccomp + rlimits only: no namespaces or Landlock, and `mmap` arguments (e.g. `PROT_EXEC`) are not restricted. Linux only (x86_64, aarch64, riscv64). On other systems `calc` is not registered.

### License

Apache-2.0. See [LICENSE](LICENSE).
