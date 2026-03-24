import os
import sys
import json
import random
import hashlib
import logging
import aiohttp
import database
import asyncio
import string
from gtts import gTTS
from dotenv import load_dotenv
from flask import Flask, request, send_from_directory
from logging.config import dictConfig

dictConfig({
    'version': 1,
    'formatters': {'default': {
        'format': '[%(asctime)s] %(levelname)s in %(module)s: %(message)s',
    }},
    'handlers': {'wsgi': {
        'class': 'logging.StreamHandler',
        'stream': 'ext://flask.logging.wsgi_errors_stream',
        'formatter': 'default'
    }},
    'root': {
        'level': os.environ.get("LOG_LEVEL_STR"),
        'handlers': ['wsgi']
    }
})

app = Flask(__name__)
load_dotenv()

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)

logging.info("Starting Wahook Client...")

logging.basicConfig(
        format='%(asctime)s %(levelname)-8s %(message)s',
        level=int(os.environ.get("LOG_LEVEL")),
        datefmt='%Y-%m-%d %H:%M:%S')
log = logging.getLogger('werkzeug')
log.setLevel(int(os.environ.get("LOG_LEVEL")))

app.logger.setLevel(int(os.environ.get("LOG_LEVEL")))

def get_waha_headers():
    headers = {
        'accept': 'application/json',
        'Content-Type': 'application/json',
        'X-Api-Key': os.environ.get("WAHA_API_KEY")
    }
    return headers

async def post_waha_request(message, msg_id, group_id):
    url = os.environ.get("WAHA_ENDPOINT")
    data = {
        'chatId': group_id,
        'reply_to': msg_id,
        'text': message,
        'linkPreview': True,
        'linkPreviewHighQuality': False,
        'session': 'default',
    }
    wahaurl = url+'/api/sendText'


    session_timeout = aiohttp.ClientTimeout(total=None,sock_connect=60,sock_read=60)
    connector = aiohttp.TCPConnector(force_close=True)
    async with aiohttp.ClientSession(connector=connector, timeout=session_timeout) as wahasession:
        async with wahasession.post(wahaurl, headers=get_waha_headers(), json=data, timeout=60) as waharesponse:
            if (waharesponse.status < 200 or waharesponse.status >= 300):
                raise Exception(waharesponse)
        
        await wahasession.close()

def get_tts_google(text: str):
    tts = gTTS(text=text, lang="it", slow=False)
    tts.save('/tmp/discord-llm-bot/wahook-audio.mp3')
    return "Non ancora implementato"
    #return "https://pezzente.fabiovalentino.it/audio/wahook-audio.mp3"

async def embed_message(text):
    try:

        m = hashlib.md5()
        m.update(text.encode('utf-8'))
        md5_text = m.hexdigest()

        data = {
            "textContent": text,
            "addToWorkspaces": os.environ.get("ANYTHING_LLM_WORKSPACE"),
            "metadata": {
                "title": "WhatsApp_sentence_" + str(md5_text)
            }
        }
        headers = {
            'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
        }
        anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT_NO_LIMIT") + "/api/v1/document/raw-text"
        connector = aiohttp.TCPConnector(force_close=True)
        session_timeout = aiohttp.ClientTimeout(total=None,sock_connect=900,sock_read=900)
        async with aiohttp.ClientSession(connector=connector, timeout=session_timeout) as anything_llm_session:
            async with anything_llm_session.post(anything_llm_url, headers=headers, json=data, timeout=900) as anything_llm_response:
                if (anything_llm_response.status != 200):
                    logging.error(anything_llm_response)
            await anything_llm_session.close()  
    except Exception:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1) 

async def start_typing(group_id):
    is_typing = False
    data = {
        "chatId": group_id,
        "session": "default"
    }
    wahaurl = os.environ.get("WAHA_ENDPOINT") + "/api/startTyping"
    session_timeout = aiohttp.ClientTimeout(total=None,sock_connect=60,sock_read=60)
    connector = aiohttp.TCPConnector(force_close=True)
    async with aiohttp.ClientSession(connector=connector, timeout=session_timeout) as wahasession:
        async with wahasession.post(wahaurl, headers=get_waha_headers(), json=data, timeout=60) as waharesponse:
            if (waharesponse.status >= 200 and waharesponse.status < 300):
                is_typing = True
            else:
                raise Exception(waharesponse)
        
        await wahasession.close()
    return is_typing


async def stop_typing(group_id):
    data = {
        "chatId": group_id,
        "session": "default"
    }
    wahaurl = os.environ.get("WAHA_ENDPOINT") + "/api/stopTyping"
    session_timeout = aiohttp.ClientTimeout(total=None,sock_connect=60,sock_read=60)
    connector = aiohttp.TCPConnector(force_close=True)
    async with aiohttp.ClientSession(connector=connector, timeout=session_timeout) as wahasession:
        async with wahasession.post(wahaurl, headers=get_waha_headers(), json=data, timeout=60) as waharesponse:
            if (waharesponse.status < 200 or waharesponse.status >= 300):
                raise Exception(waharesponse)
        
        await wahasession.close()


async def process_post_request(body):

    if body["payload"]["_data"]["body"] is not None and body["payload"]["_data"]["body"].strip() != "" and body["payload"]["_data"]["id"]["fromMe"] == False:
        if body["payload"]["_data"]["from"] == os.environ.get("WHATSAPP_GROUP_ID_1") or body["payload"]["_data"]["from"] == os.environ.get("WHATSAPP_GROUP_ID_2"):
            try:
                message = body["payload"]["_data"]["body"]
                logging.info("Received message {}".format(message))
                if message != "":
                    if message.startswith("/ask"):

                        message = message.replace("/ask", "").strip()

                        if "replyTo" in body["payload"]:
                            message = "In risposta a -> Pezzente (te stesso): " + body["payload"]["replyTo"]["body"] + ".\n\n" + message

                        
                        logging.info("Asking anythingllm: {}".format(message))

                        if message != "" and len(message) <= 500:
                            await start_typing(body["payload"]["_data"]["from"])

                            data = {
                                "message": message.rstrip(),
                                "mode": "chat",
                                "reset": "true"
                            }
                            headers = {
                                'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
                            }
                            os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"

                            anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
                            connector = aiohttp.TCPConnector(force_close=True)
                            session_timeout = aiohttp.ClientTimeout(total=None,sock_connect=900,sock_read=900)
                            async with aiohttp.ClientSession(connector=connector, timeout=session_timeout) as anything_llm_session:
                                async with anything_llm_session.post(anything_llm_url, headers=headers, json=data, timeout=900) as anything_llm_response:
                                    if (anything_llm_response.status == 200):
                                        anything_llm_json = await anything_llm_response.json()
                                        anything_llm_text = anything_llm_json["textResponse"]
                                        await post_waha_request(anything_llm_text, body["payload"]["id"], body["payload"]["_data"]["from"])

                                    elif (anything_llm_response.status == 503):
                                        await post_waha_request("Un'altra richiesta é ancora in esecuzione.\nRiprovare in un secondo momento.\nNOTA: Questo server gestisce una richiesta per volta.", body["payload"]["id"], body["payload"]["_data"]["from"])
                                    else:
                                        await  post_waha_request("Il server IA é spento. Riprovare in un secondo momento.", body["payload"]["id"], body["payload"]["_data"]["from"])
                            
                                await anything_llm_session.close()
                            
                            await stop_typing(body["payload"]["_data"]["from"]) 
                        else:
                            await post_waha_request('Se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)', body["payload"]["id"], body["payload"]["_data"]["from"])
                    elif message.startswith("/random"):
                        await start_typing(body["payload"]["_data"]["from"])

                        message = message.replace("/random", "").strip()
                        
                        sentences = None

                        if message is not None:
                            sentences = database.select_like_sentence(dbms, message)
                        else:
                            sentences = database.select_all_sentence(dbms)

                        if sentences is not None and len(sentences) > 0:         
                            text_found = random.choice(sentences)
                            await post_waha_request(text_found, body["payload"]["id"], body["payload"]["_data"]["from"])
                        else:
                            await post_waha_request("Si è verificato un errore stronzo.", body["payload"]["id"], body["payload"]["_data"]["from"])
                        await stop_typing(body["payload"]["_data"]["from"])
                    elif message.startswith("/speak"):
                        message = message.replace("/speak", "").strip()
                        if(message != "" and len(message) <= 500):
                            await start_typing(body["payload"]["_data"]["from"])
                            await post_waha_request(get_tts_google(message), body["payload"]["id"], body["payload"]["_data"]["from"])
                            await stop_typing(body["payload"]["_data"]["from"])
                            await embed_message(message)
                        else:
                            post_waha_request("Se vuoi che ripeto qualcosa devi scrivere una frase dopo /speak (massimo 500 caratteri).", body["payload"]["id"], body["payload"]["_data"]["from"])
                    elif message.startswith("/help"):
                        
                        await post_waha_request("Lista Comandi: \n- /ask <testo>: Chiedimi qualcosa.\n- /random: Frase casuale.\n- /random <testo>: Frase casuale dato un testo", body["payload"]["id"], body["payload"]["_data"]["from"])
                        #await post_waha_request("Lista Comandi: \n- /ask <testo>: Chiedimi qualcosa.\n- /speak: Parla con la voce di google.\n- /random: Frase casuale.\n- /random <testo>: Frase casuale dato un testo", body["payload"]["id"], body["payload"]["_data"]["from"])

                    elif body["payload"]["_data"]["from"] == os.environ.get("WHATSAPP_GROUP_ID_1"):
                        await embed_message(message)
            except Exception as e:
                exc_type, exc_obj, exc_tb = sys.exc_info()
                fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
                logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)



@app.route('/bot', methods=['POST'])
async def handle_webhook():
    data = request.json
    logging.debug(f"Received data: {data}")

    await process_post_request(data)
    return "OK", 200

@app.route('/audio/<path:path>')
async def send_audio(path):
    return send_from_directory('/tmp/discord-llm-bot', path)

if __name__ == '__main__':
    if int(os.environ.get("LOG_LEVEL")) <= 20:
        app.run(debug=True)
    else:
        app.run()

