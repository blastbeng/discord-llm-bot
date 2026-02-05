import os
import sys
import json
import random
import hashlib
import logging
import requests
import database
from gtts import gTTS
from dotenv import load_dotenv
from flask import Flask, request, send_from_directory

app = Flask(__name__)
load_dotenv()

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)

logging.info("Starting Wahook Client...")

logging.basicConfig(
        format='%(asctime)s %(levelname)-8s %(message)s',
        level=int(os.environ.get("LOG_LEVEL")),
        datefmt='%Y-%m-%d %H:%M:%S')

def get_waha_headers():
    headers = {
        'accept': 'application/json',
        'Content-Type': 'application/json',
        'X-Api-Key': os.environ.get("WAHA_API_KEY")
    }
    return headers

def post_waha_request(message, msg_id, group_id):
    url = os.environ.get("WAHA_ENDPOINT")
    json_data = {
        'chatId': group_id,
        'reply_to': msg_id,
        'text': message,
        'linkPreview': True,
        'linkPreviewHighQuality': False,
        'session': 'default',
    }
    url = url+'/api/sendText'


    response = requests.post(url, headers=get_waha_headers(), json=json_data, timeout=60)

    if (response.status_code != 200):
        logging.error(response.reason)

def get_tts_google(text: str):
    tts = gTTS(text=text, lang="it", slow=False)
    tts.save('/tmp/discord-llm-bot/wahook-audio.mp3')
    return "Non ancora implementato"
    #return "https://pezzente.fabiovalentino.it/audio/wahook-audio.mp3"

def compute_md5_hash(my_string):
    m = hashlib.md5()
    m.update(my_string.encode('utf-8'))
    return m.hexdigest()

def embed_message(text):
    try:
        json_data = {
            "textContent": text,
            "addToWorkspaces": os.environ.get("ANYTHING_LLM_WORKSPACE"),
            "metadata": {
                "title": "sentences_" + str(compute_md5_hash(text))
            }
        }
        headers = {
            'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
        }
        response = requests.post(os.environ.get("ANYTHING_LLM_ENDPOINT_NO_LIMIT") + "/api/v1/document/raw-text", headers=headers, json=json_data, timeout=900)
        if (response.status_code != 200):
            logging.error(response.reason)

    except Exception:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

def start_typing(group_id):
    json_data = {
        "chatId": group_id,
        "session": "default"
    }
    url = os.environ.get("WAHA_ENDPOINT") + "/api/startTyping"
    response = requests.post(url, headers=get_waha_headers(), json=json_data, timeout=60)
    if response.status_code == 200:
        return True
    return False


def stop_typing(group_id):
    json_data = {
        "chatId": group_id,
        "session": "default"
    }
    url = os.environ.get("WAHA_ENDPOINT") + "/api/stopTyping"
    response = requests.post(url, headers=get_waha_headers(), json=json_data, timeout=60)


def process_post_request(body):

    if "payload" in body and body["payload"] is not None and "id" in body["payload"] and body["payload"]["id"] is not None  and "_data" in body["payload"] and body["payload"]["_data"] is not None and "body" in body["payload"]["_data"] and body["payload"]["_data"]["body"] is not None and "from" in body["payload"]["_data"] and body["payload"]["_data"]["from"] is not None:
        if body["payload"]["_data"]["from"] == os.environ.get("WHATSAPP_GROUP_ID_1") or body["payload"]["_data"]["from"] == os.environ.get("WHATSAPP_GROUP_ID_2"):
            is_typing=False
            embed = False
            message = body["payload"]["_data"]["body"]
            try:
                logging.info("Received message {}".format(message))
                if message != "":
                    if message.startswith("/ask"):
                        is_typing=start_typing(body["payload"]["_data"]["from"])

                        message = message.replace("/ask", "").strip()

                        if message != "" and len(message) <= 500:

                            data = {
                                "message": message.rstrip(),
                                "mode": "chat",
                                "reset": "true"
                            }
                            headers = {
                                'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
                            }
                            os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
                            anything_llm_response = requests.post(os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat", headers=headers, json=data, timeout=900)

                            if (anything_llm_response.status_code == 200):
                                anything_llm_json = json.loads(anything_llm_response.text)
                                anything_llm_text = anything_llm_json["textResponse"]
                                post_waha_request(anything_llm_text, body["payload"]["id"], body["payload"]["_data"]["from"])
                            elif (anything_llm_response.status == 503):
                                post_waha_request("Un'altra richiesta é ancora in esecuzione.\nRiprovare in un secondo momento.\nNOTA: Questo server gestisce una richiesta per volta.", body["payload"]["id"], body["payload"]["_data"]["from"])
                            else:
                                post_waha_request("Il server IA é spento. Riprovare in un secondo momento.", body["payload"]["id"], body["payload"]["_data"]["from"])
                        else:
                            post_waha_request('Se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)', body["payload"]["id"], body["payload"]["_data"]["from"])
                    elif message.startswith("/random"):
                        is_typing=start_typing(body["payload"]["_data"]["from"])

                        message = message.replace("/random", "").strip()
                        
                        sentences = None

                        if message is not None:
                            sentences = database.select_like_sentence(dbms, message)
                        else:
                            sentences = database.select_all_sentence(dbms)

                        if sentences is not None and len(sentences) > 0:         
                            text_found = random.choice(sentences)
                            post_waha_request(text_found, body["payload"]["id"], body["payload"]["_data"]["from"])
                        else:
                            post_waha_request("Si è verificato un errore stronzo.", body["payload"]["id"], body["payload"]["_data"]["from"])
                    elif message.startswith("/speak"):
                        is_typing=start_typing(body["payload"]["_data"]["from"])

                        message = message.replace("/speak", "").strip()

                        if(message != "" and len(message) <= 500):
                            
                            post_waha_request(get_tts_google(message), body["payload"]["id"], body["payload"]["_data"]["from"])
                            embed = True
                        else:
                            post_waha_request("Se vuoi che ripeto qualcosa devi scrivere una frase dopo /speak (massimo 500 caratteri).", body["payload"]["id"], body["payload"]["_data"]["from"])
                    elif message.startswith("/help"):
                        
                        is_typing=start_typing(body["payload"]["_data"]["from"])
                        post_waha_request("Lista Comandi: \n- /ask <testo>: Chiedimi qualcosa.\n- /speak: Parla con la voce di google.\n- /random: Frase casuale.\n- /random <testo>: Frase casuale dato un testo", body["payload"]["id"], body["payload"]["_data"]["from"])
                    elif body["payload"]["_data"]["from"] == os.environ.get("WHATSAPP_GROUP_ID_1"):
                        embed_message(message)
            except Exception as e:
                exc_type, exc_obj, exc_tb = sys.exc_info()
                fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
                logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
            finally:
                if is_typing:
                    stop_typing(body["payload"]["_data"]["from"])
                if embed:
                    embed_message(message)



@app.route('/bot', methods=['POST'])
def handle_webhook():
    data = request.json
    logging.info(f"Dati ricevuti: {data}")

    process_post_request(data)
    return "OK", 200

@app.route('/audio/<path:path>')
def send_audio(path):
    return send_from_directory('/tmp/discord-llm-bot', path)

if __name__ == '__main__':
    app.run()