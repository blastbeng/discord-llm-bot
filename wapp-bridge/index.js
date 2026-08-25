import makeWASocket, {
    useMultiFileAuthState,
    DisconnectReason,
    fetchLatestBaileysVersion,
} from 'baileys';
import qrcode from 'qrcode-terminal';
import express from 'express';
import pino from 'pino';

const logger = pino({ level: 'silent' });

// Gate: if WAPP_ENABLED is not "true", don't connect to WhatsApp.
// The HTTP API still starts so docker healthcheck passes, but no
// QR code is shown and no messages are processed.
const ENABLED = (process.env.WAPP_ENABLED || 'false').toLowerCase() === 'true';

const app = express();
app.use(express.json({ limit: '50mb' }));

const PORT = process.env.BRIDGE_PORT || 3001;
const WEBHOOK_URL = process.env.WAPP_WEBHOOK_URL || 'http://localhost:3002/webhook';
const ALLOWED_GROUPS = (process.env.WHATSAPP_ALLOWED_GROUPS || '')
    .split(',')
    .map(g => g.trim())
    .filter(g => g.length > 0);

let sock = null;

// ─── WhatsApp Connection ───────────────────────────────────────────

async function connectToWhatsApp() {
    const { state, saveCreds } = await useMultiFileAuthState('auth_state');
    const { version } = await fetchLatestBaileysVersion();

    sock = makeWASocket({
        version,
        auth: state,
        logger,
        printQRInTerminal: true,
        browser: ['discord-llm-bot', 'Chrome', '1.0.0'],
    });

    sock.ev.on('creds.update', saveCreds);

    sock.ev.on('connection.update', (update) => {
        const { connection, lastDisconnect, qr } = update;

        if (qr) {
            console.log('\n=== WhatsApp QR Code ===');
            qrcode.generate(qr, { small: true });
            console.log('========================\n');
        }

        if (connection === 'close') {
            // lastDisconnect.error already carries an HTTP-style .output.statusCode
            // (no need for an instanceof Boom check — baileys@7 no longer exports
            // Boom). Reconnect unless the session was explicitly logged out.
            const statusCode = lastDisconnect?.error?.output?.statusCode;
            const shouldReconnect = statusCode !== DisconnectReason.loggedOut;

            if (shouldReconnect) {
                console.log('[bridge] Connection closed, reconnecting...');
                connectToWhatsApp();
            } else {
                console.log('[bridge] Connection closed permanently. Delete auth_state/ to re-scan QR.');
            }
        }

        if (connection === 'open') {
            console.log('[bridge] WhatsApp connection opened successfully!');
        }
    });

    sock.ev.on('messages.upsert', async ({ messages, type }) => {
        // Only process brand-new incoming messages ("notify"). Baileys also fires
        // this event with type "append" when loading older messages on reconnect,
        // and re-processing those would re-trigger commands (duplicate audio/TTS).
        if (type !== 'notify') return;

        for (const msg of messages) {
            if (!msg.message || msg.key.fromMe) continue;

            const from = msg.key.remoteJid;
            const isGroup = from?.endsWith('@g.us');

            // Only process messages from allowed groups
            if (isGroup && ALLOWED_GROUPS.length > 0 && !ALLOWED_GROUPS.includes(from)) {
                continue;
            }

            // Skip non-group messages if only groups are configured
            if (!isGroup && ALLOWED_GROUPS.length > 0) {
                continue;
            }

            // Extract text from message
            let text = '';
            if (msg.message.conversation) {
                text = msg.message.conversation;
            } else if (msg.message.extendedTextMessage?.text) {
                text = msg.message.extendedTextMessage.text;
            }

            if (!text || !text.startsWith('/')) continue;

            // Send webhook to the Rust bot
            const payload = {
                from: from,
                isGroup: isGroup,
                sender: msg.key.participant || msg.key.remoteJid,
                messageId: msg.key.id,
                text: text,
            };

            try {
                const webhookResp = await fetch(WEBHOOK_URL, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(payload),
                });

                // The Rust bot returns {"status":"processed","response":"..."}.
                // If there's a text response, send it back to the chat.
                if (webhookResp.ok) {
                    const data = await webhookResp.json();
                    if (data && data.response && data.response.length > 0) {
                        await sock.sendMessage(from, { text: data.response });
                    }
                }

                // Mark as read to avoid "unread" indicators
                await sock.readMessages([msg.key]);
            } catch (e) {
                console.error('[bridge] Failed to send webhook:', e.message);
            }
        }
    });
}

// ─── HTTP API for the Rust bot ─────────────────────────────────────

// POST /sendText — send a text message to a chat
app.post('/sendText', async (req, res) => {
    const { chatId, text } = req.body;
    if (!chatId || !text) {
        return res.status(400).json({ error: 'chatId and text are required' });
    }
    try {
        await sock.sendMessage(chatId, { text });
        res.json({ success: true });
    } catch (e) {
        console.error('[bridge] sendText error:', e.message);
        res.status(500).json({ error: e.message });
    }
});

// POST /sendAudio — send audio as a WhatsApp voice message.
// The Rust bot sends the audio as base64-encoded bytes (the bridge and
// the bot run in separate containers, so a file path would not be
// accessible across them). WhatsApp requires OGG Opus format for voice
// messages — we use ffmpeg to convert the audio to OGG Opus.
import { writeFileSync, readFileSync, unlinkSync } from 'fs';
import { execSync } from 'child_process';
import { tmpdir } from 'os';
import { join } from 'path';

app.post('/sendAudio', async (req, res) => {
    const { chatId, audioBase64 } = req.body;
    if (!chatId || !audioBase64) {
        return res.status(400).json({ error: 'chatId and audioBase64 are required' });
    }
    const createdFiles = [];
    try {
        // Decode base64 audio bytes to a temp file
        const audioBuffer = Buffer.from(audioBase64, 'base64');
        const inputPath = join(tmpdir(), `wapp_in_${Date.now()}.mp3`);
        writeFileSync(inputPath, audioBuffer);
        createdFiles.push(inputPath);

        // Convert to OGG Opus (WhatsApp voice message format) using ffmpeg
        const oggPath = join(tmpdir(), `wapp_${Date.now()}.ogg`);
        execSync(`ffmpeg -i "${inputPath}" -c:a libopus -b:a 64k -ac 1 -y "${oggPath}"`, {
            stdio: 'pipe',
        });
        createdFiles.push(oggPath);

        const oggBuffer = readFileSync(oggPath);

        // Send as PTT (push-to-talk / voice message)
        await sock.sendMessage(chatId, {
            audio: oggBuffer,
            mimetype: 'audio/ogg; codecs=opus',
            ptt: true,
        });

        res.json({ success: true });
    } catch (e) {
        console.error('[bridge] sendAudio error:', e.message);
        res.status(500).json({ error: e.message });
    } finally {
        // Always clean up temp files, including on error (e.g. ffmpeg failure)
        for (const f of createdFiles) {
            try { unlinkSync(f); } catch {}
        }
    }
});

// GET /status — check if WhatsApp is connected
app.get('/status', (req, res) => {
    res.json({
        connected: sock?.user ? true : false,
        user: sock?.user?.id || null,
    });
});

// ─── Start ─────────────────────────────────────────────────────────

app.listen(PORT, () => {
    console.log(`[bridge] HTTP API listening on port ${PORT}`);
    if (!ENABLED) {
        console.log('[bridge] WAPP_ENABLED is not "true" — WhatsApp connection disabled.');
        console.log('[bridge] Set WAPP_ENABLED=true in .env.wapp to enable.');
    } else {
        console.log(`[bridge] Webhook URL: ${WEBHOOK_URL}`);
        console.log(`[bridge] Allowed groups: ${ALLOWED_GROUPS.length > 0 ? ALLOWED_GROUPS.join(', ') : 'all'}`);
    }
});

if (ENABLED) {
    connectToWhatsApp();
}