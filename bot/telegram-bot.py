import logging
import os
import base64
import random
import requests
import requests_cache
import string
import sys
import urllib
import time
import aiohttp
import database
from datetime import datetime
from dotenv import load_dotenv
from io import BytesIO
from gtts import gTTS
from telegram import Update
from telegram.ext import ApplicationBuilder, CommandHandler, ContextTypes, MessageHandler, filters

load_dotenv()

requests_cache.install_cache('discord_bot_cache')

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)


TOKEN = os.environ.get("TOKEN")
CHAT_ID = os.environ.get("CHAT_ID")
GROUP_CHAT_ID = os.environ.get("GROUP_CHAT_ID")
BOT_NAME = os.environ.get("BOT_NAME")

logging.info("Starting Telegram Client...")

logging.basicConfig(
        format='%(asctime)s %(levelname)-8s %(message)s',
        level=int(os.environ.get("LOG_LEVEL")),
        datefmt='%Y-%m-%d %H:%M:%S')

application = ApplicationBuilder().token(TOKEN).build()

def get_tts_google(text: str):
    mp3_fp = BytesIO()
    tts = gTTS(text=text, lang="it", slow=False)
    tts.write_to_fp(mp3_fp)
    mp3_fp.seek(0)
    return mp3_fp

def get_anythingllm_online_status():
    try:
        r = requests.get(os.environ.get("ANYTHING_LLM_ENDPOINT_OLLAMA"), timeout=1)
        if (r.status_code == 200):
            return True
        else:
            return False
    except (requests.exceptions.ConnectionError, requests.exceptions.Timeout):
        logging.info("AnythingLLM Host Offline.")
        return False
    except requests.exceptions.HTTPError:
        logging.info("AnythingLLM Host Error 4xx or 5xx.")
        return False
    else:
        return True

def get_random_string(length):
    random_string = ''.join(random.choices(string.ascii_letters + string.digits, k=length))
    return random_string

async def echo(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            strid = "000000"
            message = update.message.text.strip()
            if(message != ""):
                if get_anythingllm_online_status():
                    data = {
                            "message": message.rstrip(),
                            "mode": "chat"
                        }
                    headers = {
                        'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
                    }
                    connector = aiohttp.TCPConnector(force_close=True)
                    anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
                    async with aiohttp.ClientSession(connector=connector) as anything_llm_session:
                        async with anything_llm_session.post(anything_llm_url, headers=headers, json=data) as anything_llm_response:
                            if (anything_llm_response.status == 200):
                                anything_llm_json = await anything_llm_response.json()
                                anything_llm_text = anything_llm_json["textResponse"].rstrip()
                                await update.message.reply_text(anything_llm_text, disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                            else:
                                await update.message.reply_text("si è verificato un errore stronzo", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        await anything_llm_session.close()  
                else:
                    await update.message.reply_text("AI API Offline, riprova piú tardi", disable_notification=True, protect_content=False)
                
            else:
                await update.message.reply_text("se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)", disable_notification=True, protect_content=False)

    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Errore!", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

application.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, echo))

async def random_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid or GROUP_CHAT_ID == chatid):
            strid = "000000"
            message = update.message.text[8:].strip()
            
            sentences = None

            if message is not None:
                sentences = database.select_like_sentence(dbms, message)
            else:
                sentences = database.select_all_sentence(dbms)

            if sentences is not None and len(sentences) > 0:            
                await update.message.reply_text(random.choice(sentences), disable_notification=True, protect_content=False)
            else:
                await update.message.reply_text("si è verificato un errore stronzo", disable_notification=True, protect_content=False)

    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Errore!", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

application.add_handler(CommandHandler('random', random_cmd))

async def ask(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
            strid = "000000"
            message = update.message.text[5:].strip()
            if(message != "" and len(message) <= 500  and not message.endswith('bot')):
                if get_anythingllm_online_status():
                    data = {
                            "message": message.rstrip(),
                            "mode": "chat"
                        }
                    headers = {
                        'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
                    }
                    connector = aiohttp.TCPConnector(force_close=True)
                    anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
                    async with aiohttp.ClientSession(connector=connector) as anything_llm_session:
                        async with anything_llm_session.post(anything_llm_url, headers=headers, json=data) as anything_llm_response:
                            if (anything_llm_response.status == 200):
                                anything_llm_json = await anything_llm_response.json()
                                anything_llm_text = anything_llm_json["textResponse"].rstrip()
                                await update.message.reply_text(anything_llm_text, disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                            else:
                                await update.message.reply_text("si è verificato un errore stronzo", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        await anything_llm_session.close()  

                else:
                    await update.message.reply_text("AI API Offline, riprova piú tardi", disable_notification=True, protect_content=False)

                
            else:
                await update.message.reply_text("se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Errore!", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

          
application.add_handler(CommandHandler('ask', ask))


async def speak(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
            strid = "000000"
            userinput = update.message.text[7:].strip()
            splitted = userinput.split("-")
            message = splitted[0].strip()
            if(message != "" and len(message) <= 500  and not message.endswith('bot')):
                audio = get_tts_google(message)
                await update.message.reply_audio(audio, disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=get_random_string(12)+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
                
            else:

                text = "se vuoi che ripeto qualcosa devi scrivere una frase dopo /speak (massimo 500 caratteri)."
                await update.message.reply_text(text, disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
               
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Errore!", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

          
application.add_handler(CommandHandler('speak', speak))

async def restart(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:

        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
            strid = "000000"    
            await update.message.reply_text("Riavvio in corso...", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

        python = sys.executable
        os.execl(python, python, *sys.argv)
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
        await update.message.reply_text("Errore!", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)


application.run_polling(allowed_updates=Update.ALL_TYPES)
