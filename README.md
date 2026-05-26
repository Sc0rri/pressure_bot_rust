# 🩺 Rust Cloudflare Worker: Pressure & Expense Logger

This is the **Rust (WebAssembly)** serverless version of **`pressure_bot`**, designed to be hosted as a **Cloudflare Worker** on their **Free Tier** ($0/month).

It processes incoming Telegram webhooks, parses blood pressure or expense inputs, recognizes blood pressure from photos using **Cloudflare Workers AI (Llama 3.2 Vision)**, and securely logs them to your Google Sheets using OAuth2 credentials—all with **~0ms cold starts** and highly optimized **KV-caching** for access tokens.

## ⚡ Features & Enhancements in Rust

1.  **📸 Advanced AI Photo OCR with Multi-Attempt Recognition:** Sends high-resolution images (`photos.last()`) of your blood pressure monitor to **Cloudflare Workers AI** (`@cf/meta/llama-3.2-11b-vision-instruct`) using **multiple parallel requests** (configurable via `AI_VISION_PARALLEL_REQUESTS`, default 4). The vision prompt requires **structured JSON output** (`{"sys": 135, "dia": 85, "pulse": 72}`), making parsing reliable. If multiple different readings are recognized, the bot presents a choice via inline keyboard buttons.
2.  **📊 Multiple Options Selection:** When AI returns different readings across attempts, the user sees all unique variants with inline buttons ("Вариант 1", "Вариант 2", ...) to pick the correct one.
3.  **💾 Strongly-Typed State Machine:** Implements a typed finite state machine (`UserState`) serialized as a JSON string in KV. State transitions live in `src/state.rs`, while Worker orchestration lives in `src/app.rs`.
4.  **⌨️ Centralized UI Button Labels:** Button labels (`✅ Save`, `❌ Cancel`, `🩺 Pressure`, `💸 Cost`) are centralized as public constants in `src/telegram.rs`, making the matching pipeline entirely typo-safe.
5.  **🛡️ Low CPU Footprint OAuth2 Caching:** Google Sheets API OAuth tokens are cached in Cloudflare KV with a 55-minute expiration. This drops average request CPU times down to **< 5ms** and ensures you stay safely under the free tier execution limits.
6.  **📦 Zero Heavy Crypto Bloat:** Uses the pure-Rust `jwt-simple` crate for fast Wasm-compatible RS256 token signing.
7.  **⏱️ Embedded Timezone Support:** Statically embeds timezone databases using Wasm-compatible `chrono-tz`, preserving precise local spreadsheet timestamps (e.g. `Europe/Kiev`). Worker logs use UTC timestamps via `log_event!`.
8.  **🌐 Cyrillic Sheets & Custom Range Encoding:** Implements native percent-encoding and automatic single-quote range escaping, handling Cyrillic tab names with spaces and parentheses (like `'Значения (2026 )'!A3`) perfectly.
9.  **📢 Direct Telegram Error Reporting:** Instantly forwards Google Sheets API errors directly to your Telegram chat to prevent silent failures.
10. **⌨️ Automatic Keyboard Management:** Seamlessly collapses and hides the Telegram custom keyboard when operations are completed or canceled without spamming "Cancelled" messages.

---

## 🧠 Operation Logic & Message Processing Flow

The bot runs a deterministic decision pipeline for every incoming message, with short-lived per-chat state stored in KV. Below is the current `UserState` machine:

```mermaid
stateDiagram-v2
    [*] --> RetrieveState
    RetrieveState --> ProcessInput : Load UserState from KV
    
    state ProcessInput {
        state "AwaitingPressureConfirmation(pending)" as APC
        state "AwaitingClassification { raw_text }" as AC
        state "AwaitingMultipleChoice { options }" as AMC
        state "UserState::None" as NoneState
        
        [*] --> CheckState
        CheckState --> APC : State Awaiting Confirmation
        CheckState --> AC : State Awaiting Classification
        CheckState --> AMC : State Awaiting Multiple Choice
        CheckState --> NoneState : No Pending State
        
        APC --> SavePressure : text == "✅ Save"
        APC --> DiscardAndProcessAPC : text != "✅ Save" (Fallback to NoneState)
        
        AC --> SaveForcedPressure : text == "🩺 Pressure"
        AC --> SaveForcedCost : text == "💸 Cost"
        AC --> DiscardAndProcessAC : text is other input (Fallback to NoneState)

        AMC --> DiscardAndProcessAMC : text input (Fallback to NoneState)
        
        NoneState --> ExecuteAction : ParserService::detect_action(text) matches
        NoneState --> AskUser : No match (unknown action)
        
        AskUser --> [*] : Save AwaitingClassification state to KV & show keyboard
        SavePressure --> [*] : Execute add_pressure & clear KV
        SaveForcedPressure --> [*] : Execute add_pressure & clear KV
        SaveForcedCost --> [*] : Execute add_cost & clear KV
    }
    
    ProcessInput --> UniversalCancel : text == "❌ Cancel"
    UniversalCancel --> [*] : Clear state in KV silently
```

### 1. Photo Recognition Flow (with Multi-Attempt Optimization)
When a user sends a photo:
- The bot downloads the highest resolution photo (`photos.last()`) from Telegram servers.
- Sends **N parallel requests** (default 4, configurable via `AI_VISION_PARALLEL_REQUESTS`) to **Cloudflare Workers AI** with an optimized prompt requiring **JSON output** (`{"sys": 135, "dia": 85, "pulse": 72}`).
- Parses each response: first tries JSON extraction, falls back to numeric text parsing.
- Collects all **unique valid** readings.
- **0 unique readings**: Shows an error and asks the user to enter pressure manually.
- **1 unique reading**: Saves as `UserState::AwaitingPressureConfirmation`, offers **✅ Save** / **❌ Cancel** keyboard.
- **2+ unique readings**: Saves as `UserState::AwaitingMultipleChoice { options }`, shows inline keyboard with "Вариант 1", "Вариант 2", ... buttons for selection.
- On selection or **✅ Save**: logs to Google Sheets Pressure tab.

### 2. Security & Access Check
Every request received at `/webhook` is authenticated. The bot verifies that the message sender's Telegram username matches the secure `ALLOWED_USERNAME` secret. Unauthorized messages are discarded instantly.

### 3. Session Lookup & State Transitions
The bot retrieves the active `UserState` from Cloudflare KV under `{chat_id}_state` and performs type-safe transitions:
*   **`UserState::AwaitingPressureConfirmation(pending)`**: If the user confirms with `✅ Save`, the data is written to Google Sheets. Any other message immediately discards this state and parses the text fresh.
*   **`UserState::AwaitingClassification { raw_text }`**: If the user selects `🩺 Pressure` or `💸 Cost`, the bot parses the *original raw text* and logs it to the corresponding tab. Any other message discards this state and processes the new text.
*   **`UserState::AwaitingMultipleChoice { options }`**: Handled via callback_query (inline buttons). Text input discards state and processes fresh.
*   **`UserState::None`**: Performs automatic text classification.

### 4. Smart Classifier (`detect_action`)
If there is no pending session (or if the input fell through), the text is processed by a highly optimized parser:
*   **🩺 Blood Pressure:** Matches if the text contains exactly **2 or 3 numbers** separated by spaces, slashes, or vertical bars (e.g., `120 80`, `130/80/70`), where numbers fit biological boundaries (systolic 80-250, diastolic 40-150, pulse 40-200). Logged to the **Pressure** tab with a timestamp.
*   **💸 Expense / Cost:** Matches if the text contains exactly **1 number** and optional text comments (e.g., `250 taxi`, `500`). Logged to the **Costs** tab with a date.
*   **❓ Ambiguous:** If the input doesn't fit either pattern (e.g., multiple numbers with text), the bot stores the raw input text in KV as `UserState::AwaitingClassification` (with a 10-minute expiration TTL) and responds with a selection keyboard asking: *"Where to save?"*.

---

## 🚀 Step-by-Step Setup & Deployment

### 1. Provision Cloudflare KV Namespace
Create the KV store namespace on Cloudflare to manage your active state and token caching:
```bash
npx wrangler kv namespace create STATE_STORE
```
*Note: If you plan on testing locally, also create a preview namespace:*
```bash
npx wrangler kv namespace create STATE_STORE --preview
```

Open your **[wrangler.toml](file:///home/alex/pressure_bot_rust/wrangler.toml)** and replace the `id` (and optionally `preview_id`) values with the output from the commands above:

```toml
[[kv_namespaces]]
binding = "STATE_STORE"
id = "YOUR_PRODUCTION_KV_NAMESPACE_ID"
preview_id = "YOUR_PREVIEW_KV_NAMESPACE_ID"  # (Optional)
```

---

### 2. Configure Cloudflare Secrets
Secrets are securely encrypted environment variables managed by Cloudflare. Run the following commands to add your configurations:

```bash
# Your Telegram Bot token from @BotFather
npx wrangler secret put BOT_TOKEN

# The Telegram username allowed to interact with the bot (without the @)
npx wrangler secret put ALLOWED_USERNAME

# The Google Spreadsheet ID
npx wrangler secret put SHEET_ID

# The full, raw JSON key contents of your Google Cloud Service Account
npx wrangler secret put GOOGLE_CREDENTIALS_JSON

# (Required for Photo Recognition) Cloudflare Account ID
npx wrangler secret put CLOUDFLARE_ACCOUNT_ID

# (Required for Photo Recognition) Cloudflare API Token with Workers AI access
npx wrangler secret put CLOUDFLARE_API_TOKEN

# (Optional Secrets) Custom Sheets and Timezone configurations
npx wrangler secret put PRESSURE_SHEET
npx wrangler secret put PRESSURE_SHEET_ID
npx wrangler secret put COSTS_SHEET
npx wrangler secret put COSTS_SHEET_ID
npx wrangler secret put TIMEZONE
```

### AI Vision Model (Optional)
By default the bot uses `@cf/meta/llama-3.2-11b-vision-instruct` (requires accepting the license). To use a different model, add as environment variable or secret:
```bash
npx wrangler secret put AI_VISION_MODEL
# Value: @cf/llava-hf/llava-1.5-7b-hf
```

### Parallel AI Recognition Requests (Optional)
By default the bot makes 4 parallel AI requests to increase accuracy. To change this:
```bash
npx wrangler secret put AI_VISION_PARALLEL_REQUESTS
# Value: 3 (or any number)
```

---

### 3. Build and Deploy
Wrangler will automatically download the required Rust target, compile your code to WebAssembly (`wasm32-unknown-unknown`), optimize the binary, and deploy it globally:

```bash
npx wrangler deploy
```

Once the deployment completes, Wrangler will output your worker's live URL (e.g., `https://pressure-bot-rust.username.workers.dev`).

---

### 4. Register the Telegram Webhook
To route your bot messages to the deployed Cloudflare Worker, point your Telegram bot webhook to your worker's `/webhook` endpoint:

```bash
curl -F "url=https://<YOUR_WORKER_URL>/webhook" https://api.telegram.org/bot<YOUR_BOT_TOKEN>/setWebhook
```

To verify that the webhook was successfully set:
```bash
curl https://api.telegram.org/bot<YOUR_BOT_TOKEN>/getWebhookInfo
```

---

## 📂 Project Structure

```
├── Cargo.toml         # Optimized dependency tree (worker, base64, jwt-simple, chrono, futures)
├── wrangler.toml      # KV bindings, AI binding, build targets, metadata
├── src/
│   ├── lib.rs         # Cloudflare Worker HTTP entrypoint
│   ├── app.rs         # Telegram update orchestration, KV state IO, photo/text/callback flow
│   ├── state.rs       # UserState, PendingPressure, pure text transition logic
│   ├── logger.rs      # UTC timestamped log_event! macro
│   ├── telegram.rs    # Telegram API models (Update, Message, PhotoSize) and service
│   ├── parser.rs      # Text parsing: blood pressure, costs, AI response (JSON + fallback)
│   ├── ai_vision.rs   # Workers AI integration: batch parallel photo recognition
│   ├── operations.rs  # Google Sheets operations (add_pressure, add_cost)
│   └── google/
│       ├── mod.rs     # Google Sheets HTTP client
│       └── auth.rs    # Google OAuth2 authentication and KV token caching
├── agents.md          # Agent instructions for AI coding assistants
└── README.md          # Setup & instruction guide
```

---

## 🔒 Git Safety & Security Guidelines

This repository is **100% safe to commit and push to public Git hosting services (like GitHub)**! 

*   **No Hardcoded Secrets:** All private keys, tokens, and credentials (`BOT_TOKEN`, `GOOGLE_CREDENTIALS_JSON`, `CLOUDFLARE_API_TOKEN`, etc.) are stored securely in Cloudflare's dashboard/CLI as **Secrets** and are never present in the source files.
*   **Safe KV Namespace IDs:** The KV namespace `id` in `wrangler.toml` is a public binding identifier and is safe to commit to Git.
*   **Pre-configured Gitignore:** The `.gitignore` is optimized for Rust and Wrangler, automatically blocking all build artifacts (`target/`, `build/`, `.wrangler/`) and local configuration files (`.dev.vars`).

---

## 📄 License

This project is licensed under the MIT License.
