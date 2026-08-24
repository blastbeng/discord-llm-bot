import makeWASocket, {
    useMultiFileAuthState,
    DisconnectReason,
    fetchLatestBaileysVersion,
} from 'baileys';
import { Boom } from 'baileys';
import qrcode from 'qrcode-terminal';
import express from 'express';
import pino from 'pino';

const logger = pino({ level: 'silent' });
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
            const shouldReconnect =
                lastDisconnect?.error instanceof Boom &&
                lastDisconnect.error.output.statusCode !== DisconnectReason.loggedOut;

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

    sock.ev.on('messages.upsert', async ({ messages }) => {
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
                await fetch(WEBHOOK_URL, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(payload),
                });
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

// POST /sendAudio — send an audio file as a WhatsApp voice message
// The Rust bot sends the file path; the bridge reads it and sends it.
// WhatsApp requires OGG Opus format for voice messages — we use ffmpeg
// to convert MP3 to OGG Opus if the file is not already in that format.
import { readFileSync, existsSync } from 'fs';
import { execSync } from 'child_process';
import { tmpdir } from 'os';
import { join } from 'path';

app.post('/sendAudio', async (req, res) => {
    const { chatId, filePath } = req.body;
    if (!chatId || !filePath) {
        return res.status(400).json({ error: 'chatId and filePath are required' });
    }
    if (!existsSync(filePath)) {
        return res.status(404).json({ error: 'File not found: ' + filePath });
    }
    try {
        // Convert MP3 to OGG Opus (WhatsApp voice message format) using ffmpeg
        const oggPath = join(tmpdir(), `wapp_${Date.now()}.ogg`);
        execSync(`ffmpeg -i "${filePath}" -c:a libopus -b:a 64k -ac 1 -y "${oggPath}"`, {
            stdio: 'pipe',
        });

        const audioBuffer = readFileSync(oggPath);

        // Send as PTT (push-to-talk / voice message)
        await sock.sendMessage(chatId, {
            audio: audioBuffer,
            mimetype: 'audio/ogg; codecs=opus',
            ptt: true,
        });

        // Clean up temp file
        try { require('fs').unlinkSync(oggPath); } catch {}

        res.json({ success: true });
    } catch (e) {
        console.error('[bridge] sendAudio error:', e.message);
        res.status(500).json({ error: e.message });
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
    console.log(`[bridge] Webhook URL: ${WEBHOOK_URL}`);
    console.log(`[bridge] Allowed groups: ${ALLOWED_GROUPS.length > 0 ? ALLOWED_GROUPS.join(', ') : 'all'}`);
});

connectToWhatsApp();