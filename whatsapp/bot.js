const { Client, LocalAuth, MessageMedia } = require('whatsapp-web.js');
const qrcode = require('qrcode-terminal');
const config = require("./config/config.json");
const axios = require('axios');
const log = require('loglevel');
const gTTS = require('gtts');
const sqlite3 = require('sqlite3').verbose();
const ERROR_MSG = "Schifo, bestemmia e disagio.\nSi é verificato un errore stronzo.\API Error? Bug Brutti? Chi lo sa!\nChiedere al dev di controllare.\nSe ha voglia lo farà."

console.log("Logging - Setting log level to: " + config.LOG_LEVEL)
log.setLevel(config.LOG_LEVEL);

client = new Client({
    //authStrategy: new LocalAuth({ dataPath: `${process.cwd()}/config/wacache` }),
    authStrategy: new LocalAuth(),
    puppeteer: { 
        headless: true,
        ignoreHTTPSErrors: true,
        args: [
            "--no-sandbox", 
            "--disable-setuid-sandbox"
        ],
        executablePath: '/usr/bin/chromium',
        //userDataDir: `${process.cwd()}/config/wacache/session`,
    }
});

client.on('loading_screen', (percent, message) => {
    console.log('LOADING SCREEN', percent, message);
});

client.on('qr', qr => {
    console.log('QR RECEIVED', qr);
    qrcode.generate(qr, {small: true});
});

client.on('authenticated', () => {
    console.log('AUTHENTICATED');
});

client.on('auth_failure', msg => {
    // Fired if session restore was unsuccessful
    console.error('AUTHENTICATION FAILURE', msg);
});

client.on('disconnected', (reason) => {
    console.log('Client was logged out', reason);
})

client.on('ready', () => {
    console.log('READY');
});

client.on('message', async msg => {
    try {
        let chat = await msg.getChat();
        if (msg.body.toLowerCase().startsWith('/help') 
            || msg.body.toLowerCase().startsWith('/random')
            || msg.body.toLowerCase().startsWith('/ask')
            || msg.body.toLowerCase().startsWith('/speak')
            ){

            let canReply = false;

            if (chat.isGroup) {
                for (let i = 0; i < chat.participants.length; i++) {
                    if (chat.participants[i].id.user === config.TEL_NUMBER){
                        canReply = true;
                        break;
                    }
                }
            }


            if (canReply) { 
                await chat.sendSeen();
                log.info("CHATID: [ "+ chat.id.user + " ], COMMAND: [ "+ msg.body + " ], ");
                if (msg.body.toLowerCase() == '/help' || msg.body.toLowerCase().startsWith('/help')) {
                    await chat.sendStateTyping();
                    let message = msg.body.slice(5);
                    helpmsg = "Lista Comandi: \n- /ask <testo>: chiedimi qualcosa\n- /speak: parla con la voce di google\n- /random: frase casuale\n- /random <testo>: frase casuale dato un testo"
                    if ( message.length === 0 ) {
                        await msg.reply(helpmsg);
                    } else {
                        await msg.reply("Sei stronzo?\nMangi le pietre o sei scemo?\nNon é necessario scrivere niente dopo /help per visualizzare i comandi disponibili.\n\n" + helpmsg);
                    }

                } else if (msg.body.toLowerCase() == '/random' || msg.body.toLowerCase().startsWith('/random')) {
                    await chat.sendStateTyping();
                    let message = msg.body.slice(7).toLowerCase();
                    var sql = "SELECT sentence FROM sentences WHERE id IN (SELECT id FROM sentences ORDER BY RANDOM() LIMIT 1)"
                    if ( message.length !== 0 ) {
                        sql = "SELECT sentence FROM sentences WHERE id IN (SELECT id FROM sentences WHERE LOWER(sentence) LIKE '%" + message + "%' OR LOWER(sentence) LIKE '" + message + "%' OR LOWER(sentence) LIKE '%" + message + "' ORDER BY RANDOM() LIMIT 1)"
                    }
                    const db = new sqlite3.Database(`${process.cwd()}/config/discord-bot.sqlite3`);
                    try {
                        db.each(sql, async (error, row) => {
                            if (error) {
                                log.error("ERRORE!", "["+ error + "]");
                                await msg.reply(ERROR_MSG);
                            } else if (row != null && 'sentence' in row && row['sentence'] != undefined && row['sentence'] != null && row['sentence'] !== ''){
                                await msg.reply(row['sentence']);
                            } else {
                                await msg.reply("Non ho trovato nessun testo contenente queste parole.");
                            }
                        });
                    } catch (error) {
                        log.error("ERRORE!", "["+ error + "]");
                        await msg.reply(ERROR_MSG);
                    } finally {
                        db.close();
                        await chat.clearState();
                    }
                }
                else if (msg.body.toLowerCase().startsWith('/ask')) {
                    let message = msg.body.slice(4);
                    if ( message.length !== 0 ) {                     
                        const {data} = await axios.post(config.ANYTHING_LLM_ENDPOINT + "/api/v1/workspace/" + config.ANYTHING_LLM_WORKSPACE + "/chat", {
                                    "message": message.trim(),
                                    "mode": "chat"
                                }, {
                                headers: {
                                    'Authorization': 'Bearer ' +config.ANYTHING_LLM_API_KEY
                                }
                            }).catch(async function(error) {
                                if (error.status >= 500) {
                                    await msg.reply("Un'altra richiesta é gia in esecuzione, per favore riprova fra qualche istante");
                                } else {
                                    log.error("ERRORE!", "["+ error + "]");
                                    await msg.reply(ERROR_MSG);
                                }
                                await chat.clearState();
                            });
                        log.info(data)
                        await msg.reply(data["textResponse"]);
                    } else {
                        await msg.reply("Sei stronzo?\nMangi le pietre o sei scemo?\nSe devi chiedermi qualcosa devi scrivere un testo dopo /ask.");
                    }
                } else if (msg.body.toLowerCase().startsWith('/speak')) {
                    await repeat(msg.body.slice(0, 6).toLowerCase(), msg.body.slice(6), msg, chat)
                }
            }
        }
    } catch (error) {
        log.error("ERRORE!", "["+ error + "]");
        await msg.reply(ERROR_MSG);
        await chat.clearState();
    }
}); 

async function repeat(command, message, msg, chat){
    
    if ( message.length !== 0 ) {
        await replyMedia(message.trim(), msg, chat)
    } else {
        await msg.reply("Sei stronzo?\nMangi le pietre o sei scemo?\nSe vuoi farmi parlare devi scrivere un testo dopo " + command + ".");
    }
}

async function replyMedia(text, msg, chat){
    await chat.sendStateRecording();
    try {
        var gtts = new gTTS(text, 'it');
        gtts.save(config.TMP_DIR + "watmpaudio.mp3", async function (error, result) {
            if(error) { 
                log.error("ERRORE!", "["+ error + "]");
                await msg.reply(ERROR_MSG);
                await chat.clearState(); 
            }
            await msg.reply(MessageMedia.fromFilePath(config.TMP_DIR + "watmpaudio.mp3"), null, { sendAudioAsVoice: true }) 
        })
    } catch (error) {
        log.error("ERRORE!", "["+ error + "]");
        await msg.reply(ERROR_MSG);
        await chat.clearState();
    }
}   

async function replyMsg(url, msg, chat){
    await chat.sendStateTyping();
    await axios.get(url).then(async function(response) {
        if(response.status === 204) {
            await msg.reply("Non ho trovato nessun testo contenente queste parole.");
        } else if(response.status === 200) {
            await msg.reply(response.data);
        } else if(response.status === 406) {
            await msg.reply("Questo testo contiene parole bloccate dai filtri.");
        } else {
            await msg.reply(ERROR_MSG);
        }
        await chat.clearState();
    }).catch(async function(error) {
        log.error("ERRORE!", "["+ error + "]");
        await msg.reply(ERROR_MSG);
        await chat.clearState();
    });
}

client.initialize();