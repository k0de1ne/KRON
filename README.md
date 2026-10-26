# kron

Один харнес для всех ИИ-подписок. Один бинарник для Windows, Linux и Android (Termux), управление через обычный чат в браузере или в приложении на телефоне.

- **Чат как в claude.ai.** Форматированные ответы с блоками кода, шаги агента сворачиваются, есть кнопка «стоп» и выбор модели. Чат ставится как приложение на Android и ПК. Открыли тот же чат на другом устройстве — видите ответ, который ещё пишется.
- **Агент с инструментами Codex CLI.** `apply_patch` взят из исходников Codex как есть, плюс `exec_command`/`write_stdin`, `read_file`, `list_dir`, `grep_files`, `update_plan`. Системный промпт тоже от Codex.
- **Подписки:**
  - ChatGPT (Codex), Claude (Claude Code), Gemini (Gemini CLI) и Antigravity — через встроенные адаптеры или через [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI);
  - у каждой подписки работают её родные возможности, например свой веб-поиск.
- **API-ключи:** DeepSeek и всё, что говорит на OpenAI-, Responses- или Anthropic-API.
- **Веб-чаты** ChatGPT и Gemini в режиме «вопрос — ответ» через браузер. Тратятся лимиты чата, а не агентские.
- **ПК, телефон и сервер связаны.** Хаб хранит чаты и логины, а агенты работают на любой подключённой машине.

## Как это устроено

```
        телефон (OnePlus) ── чат в приложении ──┐
        ПК ──────────────── чат / kron в терминале ──┤
                                                 ▼
                        hub: kron serve (сервер или ПК)
                 чаты, логины, циклы агентов, веб-интерфейс
                       │                   │
          kron node на ПК            kron node на телефоне
     (команды, файлы, браузер)        (Termux: команды, файлы)
```

Модели вызываются с хаба. Инструменты агента и браузер для веб-чатов работают на выбранной машине: в интерфейсе это переключатель «💻 машина».

## Установка

**Готовые бинарники** собирает GitHub Actions (`.github/workflows/build.yml`): Windows x64, Linux x64, Android arm64.

**Из исходников:**

```bash
cargo install --path .
```

Затем:

```bash
kron init        # пример конфига: ~/.kron/config.toml (или $KRON_HOME)
kron providers   # кто настроен и залогинен
kron serve       # хаб + чат на http://127.0.0.1:7700
```

## Телефон (OnePlus 15 / любой Android)

Есть три варианта, можно комбинировать.

**1. Телефон как экран.** Хаб живёт на сервере или ПК, на телефоне открыт чат. Это самый простой вариант.

- Откройте в Chrome `http://<хаб>:7700/?token=<токен>`, затем в меню «⋮ → Добавить на главный экран».
- Чтобы чат поставился как полноценное приложение (отдельное окно, иконка), браузер требует HTTPS. Проще всего через Tailscale: на машине с хабом выполните `tailscale serve 7700` и открывайте адрес `https://<машина>.<tailnet>.ts.net`.

**2. Телефон как рабочая машина.** Агенты выполняют команды на самом телефоне.

1. Установите Termux из F-Droid. Версия из Google Play устарела.
2. `pkg install rust clang git && git clone <repo> && cd KRON && cargo install --path .` Либо скопируйте готовый `kron-aarch64-linux-android` в `$PREFIX/bin/kron`.
3. В `~/.kron/config.toml` пропишите `[hub] url`, `token`, `node = "phone"`.
4. Запустите `termux-wake-lock`, затем `kron node --dir ~/storage/shared/Projects`. Перед этим один раз выполните `termux-setup-storage`.
5. OxygenOS агрессивно усыпляет фон. Поэтому: Настройки → Приложения → Termux → Батарея → **Не ограничивать**. В меню недавних приложений закрепите Termux замком.
6. Для автозапуска поставьте Termux:Boot и положите скрипт в `~/.termux/boot/kron`:
   ```sh
   #!/data/data/com.termux/files/usr/bin/sh
   termux-wake-lock
   kron node --dir ~/storage/shared/Projects
   ```

**3. Телефон как хаб целиком, без сервера.**

1. В Termux выполните `kron serve` (по умолчанию он слушает `127.0.0.1:7700`).
2. В Chrome откройте `http://localhost:7700`. Для localhost HTTPS не нужен, поэтому чат ставится как приложение.
3. Логины подписок делайте прямо в Termux командой `kron login …`.

## Провайдеры

| kind | Что это | Логин / ключ | Родные возможности |
|---|---|---|---|
| `codex` | ChatGPT Plus/Pro через бэкенд Codex | `~/.codex/auth.json` или `kron login codex` | web_search, freeform `apply_patch` |
| `claude` | Claude Pro/Max через OAuth Claude Code | `~/.claude/.credentials.json` или `kron login claude` | web_search |
| `gemini` | Gemini через логин Gemini CLI (Code Assist) | `~/.gemini/oauth_creds.json` или `kron login gemini` | Google Search |
| `antigravity` | Квота Google Antigravity | `kron login antigravity` | Google Search |
| `openai` | OpenAI-совместимый Chat Completions (DeepSeek, OpenRouter, локальные) | `api_key` / `api_key_env` | — |
| `responses` | OpenAI Responses API (OpenAI, CLIProxyAPI) | `api_key` / `api_key_env` | web_search |
| `anthropic` | Anthropic Messages API (Anthropic, DeepSeek `/anthropic`, CLIProxyAPI) | `api_key` / `api_key_env` | web_search (Anthropic, CLIProxyAPI) |

**Встроенные адаптеры подписок** повторяют запросы официальных клиентов. Эндпоинты, параметры OAuth и заголовки сверены с CLIProxyAPI и исходниками gemini-cli.

Как работают логины:

- Если вы уже вошли в официальный CLI на этой машине, kron использует его файл. Обновлённые токены он записывает обратно, так что официальный CLI не ломается.
- `kron login <provider>` работает и без браузера на машине (например, на сервере по SSH). Откройте ссылку на любом устройстве, войдите, затем вставьте в терминал адрес, на который вас перенаправило.
- Gemini и Antigravity требуют OAuth-клиент Google (`client_id` / `client_secret`) в конфиге провайдера. Это публичные значения из открытых исходников:
  - Gemini CLI — `packages/core/src/code_assist/oauth2.ts`;
  - Antigravity — `internal/auth/antigravity/constants.go` в CLIProxyAPI.

**Через CLIProxyAPI.** Сервисы проверяют, что к ним ходит именно официальный клиент: у Claude это десятки заголовков и TLS-отпечаток. Если встроенный адаптер начнёт получать отказы, запустите рядом с хабом CLIProxyAPI. Он поддерживается сообществом и следит за этими изменениями. Залогиньтесь через него:

```bash
cli-proxy-api --claude-login      # или --codex-login, --antigravity-login
```

Затем направьте провайдера kron на него (примеры есть в `kron init`). Родные инструменты, например веб-поиск, проходят насквозь:

```toml
[providers.claude]
kind = "anthropic"
base_url = "http://127.0.0.1:8317"
api_key = "<ключ из access.api-keys CLIProxyAPI>"
model = "claude-opus-5-5"
```

**Веб-поиск.** Инструмент `web_search` выполняет родная модель провайдера с поиском: та, что ведёт агента, или провайдер из `search = "..."` в конфиге. Так DeepSeek-агент может искать через Gemini или ChatGPT.

**DeepSeek** подключается любым из двух способов:

```toml
[providers.deepseek]
kind = "openai"
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
api_key_env = "DEEPSEEK_API_KEY"
```

```toml
[providers.deepseek-a]
kind = "anthropic"
base_url = "https://api.deepseek.com/anthropic"
model = "deepseek-chat"
api_key_env = "DEEPSEEK_API_KEY"
```

## Режимы

В чате режим выбирается в меню модели и переключателем «🛠 Агент». В терминале — флагами:

- **Агент** (по умолчанию): модель работает с инструментами на выбранной машине, пока задача не решена.
- **Чат** (`--chat`, переключатель «Агент» выключен): обычный вопрос — ответ через API или подписку.
- **Веб-чат** (`--web chatgpt|gemini`, раздел «Веб-чаты» в меню): настоящий сайт в Chrome, Chromium или Edge на машине с браузером. Один раз войдите командой `kron browser login chatgpt`. Продолжение чата в kron продолжает тот же разговор на сайте. Если сайт поменяет вёрстку, селекторы можно переопределить в `[sites.<name>]`.

Терминал:

```bash
kron "почини падающий тест"                     # агент в текущей папке
kron --chat -p claude "объясни TCP vs UDP"
kron --web gemini "придумай название для кота"
kron --on pc -t deploy "собери релиз"          # выполнить на ПК
kron                                            # интерактивный режим
kron threads / kron show <чат>
```

## Синхронизация

На хабе:

```toml
[hub]
listen = "0.0.0.0:7700"
token  = "длинный-секрет"
node   = "server"
```

На остальных машинах:

```toml
[hub]
url   = "http://server:7700"
token = "длинный-секрет"
node  = "pc"
```

Дальше `kron node` подключает машину к хабу. Обычный `kron "…"` хранит чат на хабе, а инструменты выполняет у вас. `kron sync` переносит чаты, сделанные офлайн с `--local`.

Токен даёт право выполнять команды на подключённых машинах. Через интернет держите хаб за Tailscale или WireGuard либо за HTTPS-прокси. Без токена хаб слушает только localhost.

## Свои инструменты и агенты

- Скрипт в `~/.kron/tools/` становится инструментом. Он получает JSON в stdin и отвечает в stdout. Описание кладётся рядом в `<имя>.toml`.
- Инструкции проекта берутся из `AGENTS.md`, `CLAUDE.md` или `GEMINI.md`.
- Агенты — это пресеты:

```toml
[agents.researcher]
provider = "gemini"
tools = ["web_search", "read_file"]
system = "Отвечай со ссылками на источники."
```

Один агент может отдавать подзадачи другим через `spawn_agent`.

## Важно

Адаптеры подписок **неофициальные**. Сервисы могут сломать их в любой момент, а использование может противоречить их условиям. Применяйте на свой риск.

Пока не сделано:

- песочница и подтверждение команд;
- MCP;
- картинки во входящих сообщениях;
- PTY.

Чужой код и откуда он взят перечислены в [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
