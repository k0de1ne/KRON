# kron

Один харнес для всех ИИ-подписок. Один бинарник для Windows, Linux и Android (Termux).

- **Агент** с инструментами из Codex CLI: shell, apply_patch, read_file и другие.
- **Подписки** ChatGPT (Codex), Claude (Claude Code), Gemini (Gemini CLI) и Antigravity через неофициальные адаптеры, вместе с их родными возможностями: например, у каждой свой веб-поиск.
- **API-ключи**: DeepSeek и всё, что говорит на OpenAI- или Anthropic-API (OpenRouter, llama.cpp, Ollama…).
- **Обычные чаты** ChatGPT и Gemini в режиме «вопрос — ответ» через браузер. Тратятся лимиты чата, а не агентские.
- **Синхронизация**: один хаб (сервер или ПК) хранит треды и логины, а агенты выполняют инструменты на любой подключённой машине: ПК, телефоне или сервере. С телефона можно работать через веб-интерфейс хаба.

```
kron "почини падающий тест в src/parser"          # агент в текущей папке
kron --chat -p claude "объясни разницу TCP и UDP"  # просто вопрос-ответ
kron --web chatgpt "придумай название для кота"    # чат ChatGPT через браузер
kron --on pc -t deploy "собери релиз"               # агент работает на ПК, команда с телефона
kron                                                # интерактивный режим
```

## Установка

**Готовые бинарники** собирает GitHub Actions (`.github/workflows/build.yml`): `kron-x86_64-pc-windows-msvc`, `kron-x86_64-unknown-linux-gnu`, `kron-aarch64-linux-android`.

**Из исходников** (везде одинаково, OpenSSL не нужен):

```bash
cargo install --path .
```

**Android**: в Termux выполните `pkg install rust git`, затем `cargo install --path .`. Либо скопируйте `kron-aarch64-linux-android` в `$PREFIX/bin`. Инструменты агента работают в Termux-шелле.

Затем:

```bash
kron init          # пример конфига в ~/.kron/config.toml (или $KRON_HOME)
kron providers     # кто настроен и залогинен
```

## Провайдеры

| kind | Что это | Откуда берётся логин | Родные возможности |
|---|---|---|---|
| `codex` | ChatGPT Plus/Pro через бэкенд Codex | `~/.codex/auth.json` или `kron login codex` | web_search |
| `claude` | Claude Pro/Max через OAuth Claude Code | `~/.claude/.credentials.json` или `kron login claude` | web_search |
| `gemini` | Gemini через логин Gemini CLI (Code Assist) | `~/.gemini/oauth_creds.json` или `kron login gemini` | Google Search |
| `antigravity` | Квота Google Antigravity | `kron login antigravity` | Google Search |
| `openai` | Любой OpenAI-совместимый API (DeepSeek, OpenRouter, локальные) | `api_key` / `api_key_env` | — |
| `anthropic` | Anthropic API и совместимые (DeepSeek `/anthropic`) | `api_key` / `api_key_env` | web_search (только api.anthropic.com) |

Как устроены логины:

- Если вы уже вошли в официальный CLI на этой машине, kron использует его файл как есть. Обновлённые токены он записывает обратно в том же формате, поэтому официальный CLI продолжает работать.
- `kron login <provider>` создаёт отдельный логин в `~/.kron/auth/<provider>.json`. Браузер на той же машине не нужен. Откройте ссылку где угодно, войдите, затем вставьте в терминал адрес, на который вас перенаправило. Если адрес не открылся, это нормально. Так можно залогиниться на сервере по SSH.
- На macOS Claude Code хранит токены в Keychain, поэтому там используйте `kron login claude`.
- **Gemini и Antigravity** требуют OAuth-клиент Google (`client_id` / `client_secret`), чтобы обновлять токены и выполнять `kron login`. Это публичные значения из открытых исходников:
  - Gemini CLI — файл `packages/core/src/code_assist/oauth2.ts`;
  - Antigravity — проекты-адаптеры вроде `opencode-antigravity-auth`.

  В код они не зашиты: пропишите их в конфиг провайдера. Без них Gemini работает, пока не истечёт токен из `~/.gemini/oauth_creds.json`, а официальный `gemini` сам его обновляет.

**Веб-поиск.** Инструмент `web_search` всегда выполняет родная модель провайдера, у которого поиск есть. Если текущий провайдер умеет искать сам (Codex, Claude, Gemini), используется он. Иначе, например для DeepSeek, запрос уходит провайдеру из `search = "..."` в конфиге (по умолчанию первому подходящему). DeepSeek-агент может искать через Google Gemini или через ChatGPT.

**DeepSeek** подключается любым из двух способов (оба есть в `kron init`):

```toml
[providers.deepseek]
kind = "openai"
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"          # или deepseek-reasoner
api_key_env = "DEEPSEEK_API_KEY"

[providers.deepseek-a]
kind = "anthropic"
base_url = "https://api.deepseek.com/anthropic"
model = "deepseek-chat"
api_key_env = "DEEPSEEK_API_KEY"
```

Модели указываются в конфиге (`model = ...`) и разово меняются флагом `-m`.

## Режимы

- **agent** (по умолчанию): цикл «модель → инструменты → модель», пока задача не решена.
- **`--chat`**: один ответ модели без инструментов. Это обычный чат, но через API или подписку.
- **`--web chatgpt|gemini`**: настоящий веб-чат в Chrome, Chromium или Edge с отдельным профилем в `~/.kron/browser`. Один раз выполните `kron browser login chatgpt` (откроется окно, войдите), дальше браузер работает в фоне. Продолжение треда продолжает тот же разговор на сайте. Селекторы сайтов можно переопределить в `[sites.<name>]`, если сайт поменяет вёрстку. Свой сайт добавляется так же.

Треды (`-t name`) хранят историю, и в одном треде можно менять режим и провайдера. Посмотреть историю можно командами `kron threads` и `kron show <thread>`.

## Инструменты

Набор инструментов как в Codex CLI:

| | |
|---|---|
| `shell` | команда в шелле пользователя (PowerShell на Windows, `sh` в Linux/Android) |
| `exec_command` / `write_stdin` | долгие и интерактивные процессы (серверы, REPL) |
| `apply_patch` | правка файлов в формате патчей Codex (`*** Begin Patch` …) |
| `read_file`, `list_dir`, `grep_files` | чтение и поиск с учётом `.gitignore` |
| `update_plan` | план по шагам |
| `web_search` | родной веб-поиск провайдера |
| `spawn_agent` | отдать подзадачу другому агенту из конфига (параллельно) |

Свои инструменты кладутся в `~/.kron/tools/`: любой исполняемый скрипт (`.ps1`/`.bat` на Windows). Он получает JSON-аргументы в stdin и отвечает в stdout. Описание и схему можно положить рядом в файл `<имя>.toml` с полями `name`, `description` и `parameters`. Инструкции проекта берутся из `AGENTS.md` (или `CLAUDE.md` / `GEMINI.md`) в рабочей папке.

Агенты — это именованные пресеты:

```toml
[agents.researcher]
provider = "gemini"
tools = ["web_search", "read_file"]   # пусто = все
system = "Отвечай со ссылками на источники."
```

Запуск: `kron -a researcher "…"`.

## Синхронизация: ПК + телефон + сервер

```
            ┌──────────── hub (kron serve) ─────────────┐
            │ треды, логины подписок, циклы агентов, UI │
            └───────▲───────────────▲───────────────▲───┘
         kron node  │     kron node │       браузер │ (веб-интерфейс)
                  ПК (Windows)    телефон (Termux)   телефон / любой
```

На машине-хабе (сервер или ПК, который обычно включён):

```toml
[hub]
listen = "0.0.0.0:7700"
token  = "длинный-секрет"
node   = "server"
```

```bash
kron serve
```

На остальных машинах:

```toml
[hub]
url   = "http://server:7700"
token = "длинный-секрет"
node  = "pc"          # или "phone"
```

```bash
kron node --dir ~/projects     # машина принимает работу от агентов
```

Что это даёт:

- `kron "задача"` на любой машине: тред сохраняется на хабе, а инструменты выполняются здесь, в текущей папке.
- `kron --on pc "задача"`: агент работает на ПК, даже если команду дали с телефона.
- `kron --web gemini --on pc "…"`: веб-чат идёт через браузер ПК.
- `http://server:7700/?token=…` открывает веб-интерфейс с тредами, выбором режима, модели и машины. На телефоне его можно добавить на домашний экран.
- Логины подписок хранятся только на хабе: залогиньтесь там один раз (`kron login …` по SSH), и все устройства работают через них.
- `kron sync` объединяет локальные треды (сделанные с `--local`, без сети) с хабом. Треды — это журналы с уникальными id, поэтому они сливаются без конфликтов.

Хаб слушает только HTTP. Токен даёт право выполнять команды на подключённых машинах. Через интернет держите его за Tailscale или WireGuard либо за HTTPS-прокси. Без токена хаб согласен слушать только localhost.

## Устройство

```
src/
  main.rs        CLI
  config.rs      ~/.kron/config.toml
  llm/           openai, anthropic (+ claude OAuth), codex, gemini (+ antigravity)
  auth.rs        OAuth: чтение логинов официальных CLI, обновление, kron login
  tools/         инструменты в стиле Codex
  agent.rs       цикл агента, web_search, spawn_agent
  turn.rs        один ход в режиме agent / chat / web
  browser.rs     веб-чаты через Chrome DevTools Protocol
  store.rs       треды (JSONL), слияние
  hub.rs         хаб: API, веб-интерфейс, очередь задач узлам
  node.rs        клиент хаба и воркер узла
```

## Важно

Адаптеры подписок **неофициальные**: они повторяют запросы официальных CLI и могут сломаться при изменениях на стороне сервисов. Их использование может противоречить условиям этих сервисов, поэтому применяйте на свой риск.

Пока не сделано:

- песочница и подтверждение команд (агент работает с правами пользователя);
- MCP-серверы;
- `view_image`;
- PTY для `exec_command` (используются обычные пайпы).
