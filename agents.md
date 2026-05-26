# Pressure Bot Rust - Agent Instructions

## Project Overview

**Pressure Bot Rust** is a serverless Telegram bot running as a **Cloudflare Worker** compiled to **WebAssembly** (`wasm32-unknown-unknown`). It processes incoming Telegram webhooks, parses blood pressure readings or expense inputs from text/photos, and logs them to Google Sheets via the Sheets API.

- **Language**: Rust (edition 2024)
- **Runtime**: Cloudflare Workers (WebAssembly)
- **Key integrations**: Telegram Bot API, Google Sheets API (OAuth2 / JWT), Cloudflare Workers AI vision models
- **State management**: Cloudflare KV (`STATE_STORE` namespace)
- **Build tooling**: `worker-build` via Wrangler

## Architecture

The project is a single Worker with internal library-style modules:

```text
src/
├── lib.rs            # Cloudflare Worker HTTP entrypoint
├── app.rs            # Telegram update orchestration, KV state IO, text/photo/callback flow
├── state.rs          # UserState, PendingPressure, pure text transition logic
├── logger.rs         # UTC timestamped log_event! macro
├── telegram.rs       # Telegram API types and TelegramService
├── parser.rs         # Action enum, text classifier, AI response parser
├── ai_vision.rs      # AiVisionService: parallel photo recognition requests
├── operations.rs     # Actions -> Google Sheets writes + Telegram notifications
└── google/
    ├── mod.rs        # GoogleSheetsService HTTP client
    └── auth.rs       # GoogleAuthService JWT OAuth2 token generation and KV caching
```

### Data Flow

```text
Telegram Webhook POST /webhook
  -> lib.rs: accept only POST /webhook, log request metadata, return HTTP 200-compatible empty response
  -> app.rs: parse Update, enforce ALLOWED_USERNAME, dispatch background work via ctx.wait_until()
  -> Photo? -> ai_vision.rs (N parallel requests) -> parser.rs (JSON first, fallback numeric parsing)
  -> Text?  -> KV state lookup -> state.rs text transition:
      - AwaitingPressureConfirmation -> Save or re-parse
      - AwaitingMultipleChoice       -> text discards state and re-parses
      - AwaitingClassification       -> Force Pressure/Cost or re-parse
      - None                         -> parser::detect_action(text)
  -> operations.rs -> google/auth.rs + google/mod.rs -> Google Sheets API
  -> telegram.rs -> send confirmation/error to user
```

### State Machine

`UserState` is serialized as tagged JSON in KV under key `{chat_id}_state`.

| State | Trigger to enter | Transitions out |
| --- | --- | --- |
| `None` | Default / after save or cancel | `detect_action` -> save or `AwaitingClassification` |
| `AwaitingClassification { raw_text }` | Ambiguous input | `🩺 Pressure` / `💸 Cost` / new input / `❌ Cancel` |
| `AwaitingPressureConfirmation(data)` | AI photo recognition with 1 unique option | `✅ Save` / new input / `❌ Cancel` |
| `AwaitingMultipleChoice { options }` | AI photo recognition with 2+ unique options | Inline `select_option_N` / text discard / `cancel_option` |

All KV state entries have a 10-minute TTL (`expiration_ttl(600)`).

### Photo Recognition Flow

1. Download the highest resolution photo from Telegram.
2. Send `AI_VISION_PARALLEL_REQUESTS` parallel requests to the vision model. Default: `4`.
3. Each request uses a JSON-structured prompt requiring `{"sys": 135, "dia": 85, "pulse": 72}` output.
4. Parse each response via `ParserService::parse_ai_pressure_response()`.
5. Collect unique valid `PendingPressure` readings.
6. 0 readings -> error message asking for manual input.
7. 1 reading -> `AwaitingPressureConfirmation` with Save/Cancel reply keyboard.
8. 2+ readings -> `AwaitingMultipleChoice` with inline buttons.

This is parallel sampling, not retry-after-failure logic.

## Coding Conventions

### Rust Style

- No `unwrap()` in production paths unless failure is structurally impossible. Prefer `?`, `ok_or_else`, `unwrap_or`, or explicit recovery.
- Error type is generally `worker::Error` via `worker::Result`.
- Service modules use zero-sized structs with associated functions: `ParserService`, `TelegramService`, `GoogleAuthService`, `GoogleSheetsService`, `OperationsService`, `AiVisionService`.
- Button label constants are centralized in `telegram.rs` as `pub const BTN_*`.
- Use `log_event!()` for application logs. It writes through `worker::console_log!` and prepends UTC time, level, and event name.
- Do not log raw Telegram updates, raw user text, serialized KV state, full AI responses, or tokens/secrets.
- The `#[event(fetch)]` macro in `lib.rs` is the Worker entrypoint. Background work runs via `ctx.wait_until()`.

### Environment & Secrets

Access configuration through the `Env` object.

Required secrets:
- `BOT_TOKEN`
- `ALLOWED_USERNAME`
- `SHEET_ID`
- `GOOGLE_CREDENTIALS_JSON`
- `CLOUDFLARE_ACCOUNT_ID`
- `CLOUDFLARE_API_TOKEN`

Optional secrets / vars:
- `PRESSURE_SHEET`
- `PRESSURE_SHEET_ID`
- `COSTS_SHEET`
- `COSTS_SHEET_ID`
- `TIMEZONE`
- `AI_VISION_MODEL`
- `AI_VISION_PARALLEL_REQUESTS`

Use `get_env_or_secret(env, name, default)` for values that support defaults. Never hardcode secrets in source code.

### Parser Rules

- **Pressure**: exactly 2-3 numbers with no text words. Ranges: sys 80-250, dia 40-150, pulse 40-200.
- **Cost**: exactly 1 number plus optional text comment.
- **Ambiguous**: anything else -> ask the user with the classification keyboard.
- Delimiters: whitespace, `/`, `\`, `|`.
- AI response parsing is JSON-first, then fallback numeric extraction.

### Google Sheets Integration

- OAuth tokens are cached in KV under key `google_oauth_token`.
- `google/auth.rs` owns JWT signing and OAuth token caching.
- `google/mod.rs` owns authorized Google HTTP requests.
- Pressure: inserts a row at index 1 (`batchUpdate` + `insertDimension`), then writes to `A3`.
- Costs: appends a row via the `:append` endpoint.
- Cyrillic sheet names are supported via `urlencoding::encode` and single-quote wrapping.

### Inline Keyboard Callback Data

- `confirm_pressure` - confirms pending pressure if an inline confirm flow is used.
- `cancel_pressure` - cancels pending pressure.
- `select_option_N` - selects option N from multiple choice, for example `select_option_0`.
- `cancel_option` - cancels multiple choice.
- Callback handling lives in `app.rs`.

### Logging

Prefer structured-ish event logs:

```rust
crate::log_event!("info", "telegram.text.received", "chat_id={} chars={}", chat_id, chars);
```

Logs include UTC timestamps:

```text
[2026-05-25T12:34:56.789Z] level=info event=telegram.text.received chat_id=123 chars=8
```

## Build, Test & Deploy

```bash
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings

npx wrangler build
npx wrangler deploy
npx wrangler dev
npx wrangler secret put <SECRET_NAME>
```

### Key `wrangler.toml` Bindings

- `STATE_STORE` - KV namespace for user state and Google token cache.
- `AI` - Workers AI binding is configured, but current Rust code still calls the Cloudflare AI REST endpoint using `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN`.

## Testing Guidelines

- Unit tests currently cover parser behavior, state transitions, and selected app helpers.
- Prefer adding tests around pure logic in `parser.rs`, `state.rs`, and helper functions in `app.rs`.
- Integration testing requires a deployed Worker plus Telegram webhook and real external services.

## Important Constraints

1. **Wasm compatibility**: all crates must support `wasm32-unknown-unknown`; avoid native C/system dependencies.
2. **Free tier limits**: token caching in KV is important; parallel AI requests may affect latency and usage.
3. **Always return HTTP 200-style empty responses to Telegram** after accepting webhook payloads, so Telegram does not retry handled failures.
4. **Single-user bot**: access is restricted to one `ALLOWED_USERNAME`.
5. **No `async main`**: this is a Cloudflare Worker. Entry point is `#[event(fetch)]`.

## Dependencies

| Crate | Purpose |
| --- | --- |
| `worker` | Cloudflare Workers Rust SDK |
| `worker-macros` | `#[event(fetch)]` proc macro |
| `serde` / `serde_json` | JSON serialization / deserialization |
| `chrono` / `chrono-tz` | Timestamps with timezone support |
| `jwt-simple` | RS256 JWT signing for Google OAuth2 |
| `urlencoding` | URL-encoding sheet names |
| `base64` | Base64 encoding for AI vision image payloads |
| `futures` | `join_all` for parallel AI requests |
| `http` | HTTP types |
