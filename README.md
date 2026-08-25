# discord-llm-bot

A voice TTS bot for Discord, with an optional WhatsApp bot, written in Rust (migrated from a Python implementation).

Both bots share the same SQLite sentence database and MP3 TTS cache. The Discord bot plays TTS audio in voice channels; the WhatsApp bot sends TTS audio as voice messages.

## Architecture

The project runs as three Docker Compose services:

| Service | Language | Purpose |
|---------|----------|---------|
| `discord-llm-bot` | Rust (Serenity/Poise/Songbird) | Discord slash-command bot with voice TTS |
| `whatsapp-bridge` | Node.js (Baileys) | WhatsApp connection, forwards messages to the Rust bot, sends text/audio back |
| `whatsapp-bot` | Rust (Axum) | HTTP webhook server that processes WhatsApp commands and shares the DB/TTS cache |

All three share the mounted `./config` (SQLite DB) and `./audios` (TTS cache) volumes.

## Prerequisites

- Docker + Docker Compose
- A Discord bot token and bot application created in the [Discord Developer Portal](https://discord.com/developers/applications)
- (Optional) WhatsApp: nothing external — [Baileys](https://github.com/WhiskeySockets/Baileys) connects using a QR code on first run

## Setup

1. **Discord bot config**
   ```bash
   cp .env.sample .env
   # edit .env — set BOT_TOKEN, GUILD_ID, ADMIN_ID
   ```

2. **WhatsApp config** (optional — leave disabled to skip)
   ```bash
   cp .env.wapp.sample .env.wapp
   # edit .env.wapp — set WAPP_ENABLED=true to enable
   ```

3. **Start everything**
   ```bash
   docker compose up -d --build
   ```

4. **Enable WhatsApp**: With `WAPP_ENABLED=true`, run `docker compose logs -f whatsapp-bridge` and scan the printed QR code once with the phone. The session is persisted in `wapp-bridge/auth_state/`.

> **Note:** With `WAPP_ENABLED=false` (default), the WhatsApp services stay running but idle — they do not affect the Discord bot.

## Discord Commands

| Command | Description |
|---------|-------------|
| `/join` | Join your voice channel |
| `/leave` | Leave the voice channel |
| `/stop` | Stop current playback |
| `/speak <text> [voice] [effect]` | Speak text via TTS |
| `/random [text] [voice] [effect]` | Play a random sentence (optionally search) |
| `/ask <question> [voice] [effect]` | Ask an LLM (requires config) |
| `/translate <text> <lang> [voice] [effect]` | Translate and speak via LLM |
| `/joke [voice] [effect]` | Fetch and speak a random joke |
| `/audio <file>` | Play an uploaded audio file |
| `/volume <0-100>` | Set playback volume |
| `/stats` | Show bot statistics |
| `/help` | Interactive help |
| `/restart`, `/rename`, `/avatar` | Admin-only (parent server) |

## WhatsApp Commands

```
/speak <text> [--voice Google] [--effect none]
/random [search] [--voice Google] [--effect none]
/ask <question>
/translate <text> <lang>
/joke
/stats
/help
```

## Environment Variables

**Discord (`.env`)**: `BOT_TOKEN`, `GUILD_ID`, `ADMIN_ID`, `LANG`, `LOG_LEVEL`, `TMP_DIR`, `SAVE_MP3_ON_DISK`, `MAX_AUDIO_FILE_SIZE_MB`, plus optional `FAKEYOU_USERNAME`/`FAKEYOU_PASSWORD` and LLM (`LLM_ENDPOINTS`, `LLM_API_KEYS`, `LLM_MODELS`).

**WhatsApp (`.env.wapp`)**: `WAPP_ENABLED`, `BRIDGE_PORT`, `WAPP_WEBHOOK_URL`, `WHATSAPP_ALLOWED_GROUPS`, `WAPP_WEBHOOK_PORT`, `WAPP_BRIDGE_URL`, plus the same shared DB/TTS/LLM/FakeYou settings.

## Voices & Effects

- **Voices**: Google, plus FakeYou voices (Goku, Gerry Scotti, Homer Simpson, Peter Griffin, Papa Francesco, Silvio Berlusconi). Use `--voice random`.
- **Effects**: `none`, `echo`, `reverb`, `bass`, `chipmunk`, `demon`, `telephone`, `underwater`. Use `--effect random`.

## Running without Docker (Discord only)

```bash
./build.sh        # or ./build.sh release
./run.sh          # or ./run.sh release
```

## License

MIT
