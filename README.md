# 🩺 Rust Cloudflare Worker: Pressure & Expense Logger

This is the **Rust (WebAssembly)** serverless version of **`pressure_bot`**, designed to be hosted as a **Cloudflare Worker** on their **Free Tier** ($0/month).

It processes incoming Telegram webhooks, parses blood pressure or expense inputs, and securely logs them to your Google Sheets using OAuth2 credentials—all with **~0ms cold starts** and highly optimized **KV-caching** for access tokens.

---

## ⚡ Features & Enhancements in Rust

1.  **💾 Serverless State Management:** Replaces the Go in-memory map with **Cloudflare KV**, making the entire bot completely stateless and scalable.
2.  **🛡️ Low CPU Footprint OAuth2 Caching:** Google Sheets API OAuth tokens are cached in Cloudflare KV with a 55-minute expiration. This drops average request CPU times down to **< 5ms** and ensures you stay safely under the free tier execution limits.
3.  **📦 Zero Heavy Crypto Bloat:** Uses the pure-Rust `jwt-simple` crate for fast Wasm-compatible RS256 token signing.
4.  **⏱️ Embedded Timezone Support:** Statically embeds timezone databases using Wasm-compatible `chrono-tz`, preserving your precise local logging times (e.g. `Europe/Kiev`).
5.  **🌐 Cyrillic Sheets & Custom Range Encoding:** Implements native percent-encoding and automatic single-quote range escaping, handling Cyrillic tab names with spaces and parentheses (like `'Значения (2026 )'!A3`) perfectly.
6.  **📢 Direct Telegram Error Reporting:** Instantly forwards Google Sheets API errors directly to your Telegram chat to prevent silent failures.
7.  **⌨️ Automatic Keyboard Management:** Seamlessly collapses and hides the Telegram custom keyboard when operations are completed or canceled.

---

## 🧠 Operation Logic & Message Processing Flow

The bot runs a completely stateless, deterministic decision pipeline for every incoming message. Below is the exact step-by-step logic:

```mermaid
graph TD
    A[Incoming Message] --> B{Access Control: Username Allowed?}
    B -- No --> C[Silent Drop]
    B -- Yes --> D{Pending Action in KV?}
    
    D -- Yes --> E{Matches Confirm Buttons?}
    E -- 🩺 Pressure --> F1[Parse pending text as Pressure & Save]
    E -- 💸 Cost --> F2[Parse pending text as Cost & Save]
    E -- ❌ Cancel --> F3[Delete pending KV state]
    E -- No (other text) --> G[Proceed to Classification]
    
    D -- No --> G
    
    G --> H{Classifier detect_type}
    H -- "Pressure (2-3 nums)" --> I[Save to Pressure Sheet]
    H -- "Cost (1 num + optional text)" --> J[Save to Costs Sheet]
    H -- "Ambiguous / Multi-num" --> K[Save text in KV for 10 min & Send confirm keyboard]

    F1 --> L[Clear KV State & Auto-collapse keyboard]
    F2 --> L
    F3 --> L
    I --> L
    J --> L
```

### 1. Security & Access Check
Every request received at `/webhook` is authenticated. The bot verifies that the message sender's Telegram username matches the secure `ALLOWED_USERNAME` secret. Unauthorized messages are discarded instantly.

### 2. Session Lookup (Cloudflare KV)
The bot checks Cloudflare KV under the sender's `chat_id` key for any previously stored ambiguous messages:
*   If a pending text exists and the user clicked **🩺 Pressure** or **💸 Cost**, the bot executes the respective action on the *stored pending text*, clears the KV state, and collapses the reply keyboard.
*   If the user clicked **❌ Cancel**, the KV state is cleared, and the keyboard is collapsed.
*   If the user sends any other message, it falls through to new input classification.

### 3. Smart Classifier (`detect_type`)
If there is no pending session (or if the input fell through), the text is processed by a highly optimized parser:
*   **🩺 Blood Pressure:** Matches if the text contains exactly **2 or 3 numbers** separated by spaces, slashes, or vertical bars (e.g., `120 80`, `130/80/70`), where numbers fit biological boundaries (systolic 80-250, diastolic 40-150, pulse 40-200). Logged to the **Pressure** tab with a timestamp.
*   **💸 Expense / Cost:** Matches if the text contains exactly **1 number** and optional text comments (e.g., `250 taxi`, `500`). Logged to the **Costs** tab with a date.
*   **❓ Ambiguous:** If the input doesn't fit either pattern (e.g., multiple numbers with text), the bot stores the raw input text in KV (with a 10-minute expiration TTL) and responds with a selection keyboard asking: *"Where to save?"*.

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

# (Optional Secrets) Custom Sheets and Timezone configurations
npx wrangler secret put PRESSURE_SHEET
npx wrangler secret put PRESSURE_SHEET_ID
npx wrangler secret put COSTS_SHEET
npx wrangler secret put COSTS_SHEET_ID
npx wrangler secret put TIMEZONE
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
├── Cargo.toml      # Optimized dependency tree (jwt-simple, chrono, serde)
├── wrangler.toml   # KV bindings, build targets, and metadata
├── src/
│   └── lib.rs      # Pure Rust event handler, parsing engine, and Sheets APIs
└── README.md       # Deletion & Setup instruction guide
```

---

## 🔒 Git Safety & Security Guidelines

This repository is **100% safe to commit and push to public Git hosting services (like GitHub)**! 

*   **No Hardcoded Secrets:** All private keys, tokens, and credentials (`BOT_TOKEN`, `GOOGLE_CREDENTIALS_JSON`, etc.) are stored securely in Cloudflare's dashboard/CLI as **Secrets** and are never present in the source files.
*   **Safe KV Namespace IDs:** The KV namespace `id` in `wrangler.toml` is a public binding identifier and is safe to commit to Git.
*   **Pre-configured Gitignore:** The `.gitignore` is optimized for Rust and Wrangler, automatically blocking all build artifacts (`target/`, `build/`, `.wrangler/`) and local configuration files (`.dev.vars`).

---

## 📄 License

This project is licensed under the MIT License.