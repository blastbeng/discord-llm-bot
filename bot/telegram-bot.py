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
import hashlib
import database
import wave
import uuid
from datetime import datetime
from os.path import join, dirname
from dotenv import load_dotenv
from io import BytesIO
from gtts import gTTS
from telegram import Update
from telegram.ext import ApplicationBuilder, CommandHandler, ContextTypes, MessageHandler, filters
from piper.voice import PiperVoice
from pydub import AudioSegment
from gradio_client import Client, handle_file

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

def get_framepack_online_status():
    try:
        r = requests.get(os.environ.get("FRAMEPACK_ENDPOINT"), timeout=1)
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
        if(CHAT_ID == chatid):
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

async def generate_from_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    await generate(update, context, from_cmd=True)

async def generate_from_video(update: Update, context: ContextTypes.DEFAULT_TYPE):
    await generate(update, context, from_cmd=False, model=random.choice(["Video","Video F1"]), is_video=True)

async def generate(update: Update, context: ContextTypes.DEFAULT_TYPE, from_cmd=False, model=random.choice(["Original","F1"]), is_video=False):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            strid = "000000"
            message = update.message.text[10:].strip() if from_cmd else update.message.caption
            if(len(message) <= 500  and not message.endswith('bot')):
                if get_anythingllm_online_status() and get_framepack_online_status():
                    prompt = None
                    if message == "" or message == None:
                        data = {
                            "message": "Mi serve un prompt casuale da usare in un generatore AI text-to-video o img-to-video",
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
                                    prompt = anything_llm_json["textResponse"].rstrip()
                                else:
                                    raise Exception("Prompt from AnythingLLM is None")
                            await anything_llm_session.close()
                    else:
                        prompt = message

                    photo = None
                    video = None
                    if is_video:
                        content = await update.message.effective_attachment.get_file()
                        video = {"video":handle_file(content.file_path)}
                    else:
                        content = await update.message.effective_attachment[-1].get_file()
                        photo = handle_file(content.file_path)

                    client = Client(os.environ.get("FRAMEPACK_ENDPOINT"))
                    result = client.predict(
                            selected_model=model,
                            param_1=photo,
                            param_2=video,
                            param_3=None,
                            param_4=1,
                            param_5=prompt,
                            param_6="",
                            param_7=random.randint(0,9223372036854775807),
                            param_8=False,
                            param_9=1,
                            param_10=9,
                            param_11=25,
                            param_12=1,
                            param_13=10,
                            param_14=0,
                            param_15="MagCache",
                            param_16=25,
                            param_17=0.15,
                            param_18=0.1,
                            param_19=2,
                            param_20=0.25,
                            param_21=4,
                            param_22="Noise",
                            param_23=True,
                            param_24=[],
                            param_25=384,
                            param_26=640,
                            param_27=True,
                            param_28=5,
                            api_name="/handle_start_button"
                    )
                    monitor_result = client.predict(
                        job_id=result[1],
                        api_name="/monitor_job"
                    )
                    if monitor_result[3] == "Completed" and monitor_result[0]["video"] is not None:
                        await update.message.reply_video(monitor_result[0]["video"], caption=prompt, show_caption_above_media=True, width=384, height=640, disable_notification=True, filename=get_random_string(12)+ "video.mp4", reply_to_message_id=update.message.message_id, protect_content=False)
                    else:
                        await update.message.reply_text("La generazione di questo video é stata interrotta", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

                else:
                    await update.message.reply_text("AI API Offline, riprova piú tardi", disable_notification=True, protect_content=False)

                
            else:
                await update.message.reply_text("se vuoi che genero video devi scrivere una frase dopo /generate (massimo 500 caratteri)", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Errore!", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

          
application.add_handler(CommandHandler('generate', generate_from_cmd))

application.add_handler(MessageHandler(filters.PHOTO, generate))

application.add_handler(MessageHandler(filters.VIDEO, generate_from_video))

application.add_handler(MessageHandler(filters.ANIMATION, generate_from_video))

async def speak(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
            strid = "000000"
            userinput = update.message.text[7:].strip()
            splitted = userinput.split("-")
            message = splitted[0].strip()
            if(message != "" and len(message) <= 500  and not message.endswith('bot')):
                if len(splitted) > 1 and splitted[1] is not None and lower(splitted[1].strip() != 'google'):
                    voice_str = splitted[1].strip()
                    model = join(dirname(__file__), "models/" + voice_str + '.onnx') 
                    if os.path.isfile(model):
                        voice = PiperVoice.load(model)
                        file_path = os.environ.get("TMP_DIR") + str(uuid.uuid4()) + ".wav"
                        with wave.open(file_path, "w") as wav_file:
                            voice.synthesize(message, wav_file)
                        audio = AudioSegment.from_wav(file_path)
                        out = BytesIO()
                        audio.export(out, format='mp3', bitrate="256k")
                        out.seek(0)
                        os.remove(file_path)
                        await update.message.reply_audio(out, disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=get_random_string(12)+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
                    else:
                        await update.message.reply_text("Voce " + voice_str + " non trovata!", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                else:
                    await update.message.reply_audio(get_tts_google(message), disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=get_random_string(12)+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
                #await embed_message(message)
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
