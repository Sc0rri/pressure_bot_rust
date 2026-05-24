# Pressure Bot Rust — Agent Instructions

## Project Overview

**Pressure Bot Rust** is a serverless Telegram bot running as a **Cloudflare Worker** compiled to **WebAssembly** (`wasm32-unknown-unknown`). It processes incoming Telegram webhooks, parses blood pressure readings or expense inputs from text/photos, and logs them to Google Sheets via the Sheets API.

- **Language**: Rust (edition 2024)
- **Runtime**: Cloudflare Workers (WebAssembly)
- **Key Integrations**: Telegram Bot API, Google Sheets API (OAuth2 / JWT), Cloudflare Workers AI (vision models)
- **State Management**: Cloudflare KV (`STATE_STORE` namespace)
- **Build Tooling**: `worker-build` → Wrangler

## Architecture

```
src/
├── lib.rs          # Entry point: HTTP handler, state machine orchestration, routing
├── telegram.rs     # Telegram API types (Update, Message, CallbackQuery, PhotoSize) & TelegramService
├── parser.rs       # Text classification: Action enum (Pressure | Cost), detect_action, AI response parsing (JSON + fallback)
├── ai_vision.rs    # AiVisionService: batch parallel photo recognition via Cloudflare Workers AI
├── operations.rs   # OperationsService: dispatches Actions → Google Sheets writes + Telegram notifications
└── google.rs       # GoogleSheetsService: JWT-based OAuth2 token generation, KV caching, Sheets HTTP client
```

### Data Flow

```
Telegram Webhook POST /webhook
  → lib.rs: parse Update → access control (ALLOWED_USERNAME)
  → Photo? → ai_vision.rs (Workers AI OCR, N parallel attempts) → parser.rs (parse_ai_pressure_response: JSON first, then fallback)
  → Text?  → KV state lookup → state machine transition:
      • AwaitingPressureConfirmation → Save or re-parse
      • AwaitingMultipleChoice      → Inline button selection (select_option_N / cancel_option)
      • AwaitingClassification      → Force Pressure/Cost or re-parse
      • None                        → parser::detect_action(text)
  → operations.rs → google.rs → Google Sheets API
  → telegram.rs → send confirmation/error to user
```

### State Machine (`UserState`)

The bot implements a **type-safe finite state machine** serialized as tagged JSON in KV under key `{chat_id}_state`:

| State                                | Trigger to Enter                  | Transitions Out                             |
|--------------------------------------|-----------------------------------|---------------------------------------------|
| `None`                                | Default / after save or cancel    | → detect_action → save or → AwaitingClassification |
| `AwaitingClassification { raw_text }`  | Ambiguous input                   | `🩺 Pressure` / `💸 Cost` / new input / `❌ Cancel` |
| `AwaitingPressureConfirmation(data)`  | AI photo recognition (1 unique)   | `✅ Save` / new input / `❌ Cancel`          |
| `AwaitingMultipleChoice { options }`  | AI photo recognition (2+ unique)  | Inline button `select_option_N` / text discard / `cancel_option` |

All KV state entries have a **10-minute TTL** (`expiration_ttl(600)`).

### Photo Recognition Flow (Multi-Attempt Optimization)

1. Download highest resolution photo from Telegram
2. Send **N parallel requests** (default 4, configurable via `AI_VISION_RETRIES` env) to Workers AI
3. Each request uses JSON-structured prompt requiring `{"sys": 135, "dia": 85, "pulse": 72}` output
4. Parse each response via `parse_ai_pressure_response()` (JSON first, then numeric fallback)
5. Collect unique valid `PendingPressure` readings
6. 0 readings → error message
7. 1 reading → `AwaitingPressureConfirmation` with Save/Cancel keyboard
8. 2+ readings → `AwaitingMultipleChoice` with inline buttons for selection

## Coding Conventions

### Rust Style
- **No `unwrap()` in production paths.** Use `?` operator or explicit error handling.
- Error types: use `worker::Error` (via `worker::Result`). Convert with `worker::Error::from(string)`.
- All services are **zero-sized structs** with `impl` blocks of associated functions (no `self`). Example: `ParserService`, `TelegramService`, `GoogleSheetsService`, `OperationsService`, `AiVisionService`.
- Button label constants are centralized in `telegram.rs` as `pub const BTN_*`.
- Use `console_log!()` for logging — this is a Cloudflare Worker, not `println!`.
- The `#[event(fetch)]` macro is the entry point; background work runs via `ctx.wait_until()` to return HTTP 200 immediately.

### Environment & Secrets
Access configuration through the `Env` object:
- **Secrets** (via Wrangler CLI): `BOT_TOKEN`, `ALLOWED_USERNAME`, `SHEET_ID`, `GOOGLE_CREDENTIALS_JSON`, `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_API_TOKEN`
- **Optional secrets**: `PRESSURE_SHEET`, `PRESSURE_SHEET_ID`, `COSTS_SHEET`, `COSTS_SHEET_ID`, `TIMEZONE`, `AI_VISION_MODEL`, `AI_VISION_RETRIES`
- Use `get_env_or_secret(env, name, default)` helper to read with fallback defaults.
- **Never hardcode secrets** in source code.

### Parser Rules
- **Pressure**: exactly 2–3 numbers with no text words. Ranges: sys 80–250, dia 40–150, pulse 40–200.
- **Cost**: exactly 1 number + optional text comment.
- **Ambiguous**: anything else → ask user with keyboard.
- Delimiters: whitespace, `/`, `\`, `|`.
- **AI response parsing**: JSON-first (look for `{...}` with `sys`, `dia`, `pulse` keys), then fallback to numeric extraction from text.

### Google Sheets Integration
- OAuth tokens are **cached in KV** under key `google_oauth_token` with ~55 min TTL.
- Pressure: inserts a row at index 1 (`batchUpdate` + `insertDimension`), then writes to `A3`.
- Costs: appends a row via `:append` endpoint.
- Cyrillic sheet names are supported via `urlencoding::encode` + single-quote wrapping.

### Inline Keyboard Callback Data
- `confirm_pressure` — confirms pending pressure (from `AwaitingPressureConfirmation`)
- `cancel_pressure` — cancels pending pressure
- `select_option_N` — selects option N from multiple choice (e.g. `select_option_0`, `select_option_1`)
- `cancel_option` — cancels multiple choice
- All handled in `handle_callback_query()` in `lib.rs`

### Telegram Service: `send_inline_message`
- Used for sending messages with inline keyboards (not reply keyboards)
- Used in multiple choice selection flow
- `choice_keyboard(options_count)` generates buttons: "Вариант 1", "Вариант 2", ... + "❌ Cancel"

## Build & Deploy

```bash
# Build (compiles Rust → Wasm)
npx wrangler build

# Deploy to Cloudflare
npx wrangler deploy

# Run locally
npx wrangler dev

# Manage secrets
npx wrangler secret put <SECRET_NAME>
```

### Key `wrangler.toml` Bindings
- `STATE_STORE` — KV namespace for user state and token cache
- `AI` — Workers AI binding

## Testing Guidelines

- There are currently **no unit tests** in the project. When adding new functionality, consider adding unit tests for parsers and pure logic.
- `ParserService` methods are pure functions — ideal for unit testing without mocking.
- Integration testing requires a deployed worker + Telegram webhook.

## Important Constraints

1. **Wasm Compatibility**: All crates must support `wasm32-unknown-unknown`. No native C/system dependencies. The `jwt-simple` crate uses `pure-rust` feature for this reason.
2. **Free Tier Limits**: Keep CPU time under 10ms per request. Token caching in KV is critical for this. Batch AI requests may increase CPU time — monitor usage.
3. **Always return HTTP 200 to Telegram**: Even on errors — otherwise Telegram retries the webhook indefinitely.
4. **Single user bot**: Access is restricted to one `ALLOWED_USERNAME`. This is not a multi-tenant system.
5. **No `async main`**: This is a Cloudflare Worker, not a standalone binary. Entry point is `#[event(fetch)]`.

## Dependencies

| Crate         | Purpose                                    |
|---------------|--------------------------------------------|
| `worker`      | Cloudflare Workers Rust SDK                |
| `worker-macros` | `#[event(fetch)]` proc-macro             |
| `serde` / `serde_json` | JSON serialization / deserialization |
| `chrono` / `chrono-tz` | Timestamps with timezone support     |
| `jwt-simple`  | RS256 JWT signing for Google OAuth2        |
| `urlencoding` | URL-encoding Cyrillic sheet names          |
| `base64`      | Base64 encoding for AI vision image payloads |
| `futures`     | `join_all` for parallel AI requests        |
| `http`        | HTTP types                                 |