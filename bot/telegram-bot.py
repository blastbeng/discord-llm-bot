import logging
import os
import base64
import random
import requests
import string
import sys
import urllib
import time
import aiohttp
import hashlib
import database
import wave
import asyncio
import uuid
import aiofiles
from asyncio import run
from threading import Thread
from datetime import datetime
from os.path import join, dirname
from dotenv import load_dotenv
from io import BytesIO
from gtts import gTTS
from telegram import Update, Bot
from telegram.ext import ApplicationBuilder, CommandHandler, ContextTypes, MessageHandler, filters, Defaults
from piper.voice import PiperVoice
from pydub import AudioSegment
from PIL import Image
import moviepy

load_dotenv()

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)


TOKEN = os.environ.get("TOKEN")
CHAT_ID = os.environ.get("CHAT_ID")
GROUP_CHAT_ID = os.environ.get("GROUP_CHAT_ID")
BOT_NAME = os.environ.get("BOT_NAME")
TIMEOUT = 21600

logging.info("Starting Telegram Client...")

logging.basicConfig(
        format='%(asctime)s %(levelname)-8s %(message)s',
        level=int(os.environ.get("LOG_LEVEL")),
        datefmt='%Y-%m-%d %H:%M:%S')

df = Defaults(block=False)
application = ApplicationBuilder().token(TOKEN).connect_timeout(180).defaults(df).build()

def get_tts_google(text: str):
    mp3_fp = BytesIO()
    tts = gTTS(text=text, lang="it", slow=False)
    tts.write_to_fp(mp3_fp)
    mp3_fp.seek(0)
    return mp3_fp

def get_anythingllm_online_status():
    try:
        r = requests.get(os.environ.get("ANYTHING_LLM_ENDPOINT"), timeout=5)
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

def get_aivg_online_status():
    try:
        r = requests.get(os.environ.get("AIVG_ENDPOINT"), timeout=5)
        if (r.status_code == 200):
            return True
        else:
            return False
    except (requests.exceptions.ConnectionError, requests.exceptions.Timeout):
        logging.info("AIVG Host Offline.")
        return False
    except requests.exceptions.HTTPError:
        logging.info("AIVG Host Error 4xx or 5xx.")
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
                    anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
                    connector = aiohttp.TCPConnector(force_close=True)
                    async with aiohttp.ClientSession(connector=connector) as anything_llm_session:
                        async with anything_llm_session.post(anything_llm_url, headers=headers, json=data) as anything_llm_response:
                            if (anything_llm_response.status == 200):
                                anything_llm_json = await anything_llm_response.json()
                                anything_llm_text = anything_llm_json["textResponse"].rstrip()
                                await update.message.reply_text(anything_llm_text, disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                            else:
                                logging.error(anything_llm_response)
                                await update.message.reply_text("si è verificato un errore stronzo", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        await anything_llm_session.close()  
                else:
                    await update.message.reply_text("AI API Offline oppure un altra richiesta é ancora in corso, riprova piú tardi", disable_notification=True, protect_content=False)
                
            else:
                await update.message.reply_text("se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)", disable_notification=True, protect_content=False)

    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      

application.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, echo))

async def random_cmd(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid or GROUP_CHAT_ID == chatid):
            
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
    
      

application.add_handler(CommandHandler('random', random_cmd))

async def ask(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            
            message = update.message.text[5:].strip()
            if(len(message) <= 500  and not message.endswith('bot')):
                if get_anythingllm_online_status():
                    connector = aiohttp.TCPConnector(force_close=True)
                    anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
                    data = {
                            "message": message.rstrip(),
                            "mode": "chat"
                        }
                    headers = {
                        'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
                    }
                    async with aiohttp.ClientSession(connector=connector) as anything_llm_session:
                        async with anything_llm_session.post(anything_llm_url, headers=headers, json=data) as anything_llm_response:
                            if (anything_llm_response.status == 200):
                                anything_llm_json = await anything_llm_response.json()
                                anything_llm_text = anything_llm_json["textResponse"].rstrip()
                                await update.message.reply_text(anything_llm_text, disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                            else:
                                await update.message.reply_text(r.reason + " - Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", disable_notification=True, protect_content=False)
                        await anything_llm_session.close()  

                else:
                    await update.message.reply_text("AI API Offline oppure un altra richiesta é ancora in corso, riprova piú tardi", disable_notification=True, protect_content=False)

                
            else:
                await update.message.reply_text("se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico, riprovare in un secondo momento", disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      
          
application.add_handler(CommandHandler('ask', ask))


def download_image(url, file_path=os.environ.get("TMP_DIR")):
    final_path = None
    filename, file_extension = os.path.splitext(os.path.basename(url))
    full_path = file_path + filename + file_extension
    urllib.request.urlretrieve(url, full_path)
    if file_extension != ".png":
        im = Image.open(full_path)
        final_path = file_path + filename + ".png"
        im.save(final_path)
    else:
        final_path = full_path
    return final_path

def download_video(url, file_path=os.environ.get("TMP_DIR")):
    final_path = None
    filename, file_extension = os.path.splitext(os.path.basename(url))
    full_path = file_path + filename + file_extension
    urllib.request.urlretrieve(url, full_path)
    if file_extension != ".mp4":
        clip = moviepy.editor.VideoFileClip(full_path)
        final_path = file_path + filename + ".mp4"
        clip.write_videofile(final_path)
    else:
        final_path = full_path
    return final_path

async def ask_for_generation(message_id, url, image, video, from_scheduler=False):
    connector = aiohttp.TCPConnector(force_close=True)
    async with aiohttp.ClientSession(connector=connector) as session:
        try:
            files = None
            if image:
                files = {'image': open(download_image(image),'rb')}
            elif video:
                files = {'video': open(download_video(video),'rb')}
            async with session.post(url, timeout=TIMEOUT, data=files) as response:
                if response.status == 200:
                    async with aiofiles.open(f'{os.environ.get("TMP_DIR")}video.mp4', 'wb') as f:
                        async for chunk in response.content.iter_chunked(4096):
                            await f.write(chunk)
                        time.sleep(5)
                    caption = ""
                    for keyh, valueh in response.headers.items():
                        if keyh.startswith("X-FramePack-") and keyh != "X-FramePack-Prompt":
                            caption = caption + keyh.replace("X-FramePack-","") + ": " + valueh + "\n"
                    await Bot(TOKEN).sendVideo(video=f'{os.environ.get("TMP_DIR")}video.mp4', show_caption_above_media=True, chat_id=CHAT_ID, caption=caption, filename=str(uuid.uuid4())+".mp4",  disable_notification=True, protect_content=False, reply_to_message_id=message_id)
                elif response.status == 206 and not from_scheduler:
                    await Bot(TOKEN).sendMessage(text="Un altra generazione é ancora in corso, riprovare in un secondo momento", chat_id=CHAT_ID, disable_notification=True, protect_content=False, reply_to_message_id=message_id)
                elif not from_scheduler:
                    await Bot(TOKEN).sendMessage(text="Si é verificato un errore nella richiesta", chat_id=CHAT_ID, disable_notification=True, protect_content=False, reply_to_message_id=message_id)

        except Exception as e:
            await Bot(TOKEN).sendMessage(text=str(e) + " - Si é verificato un errore nella richiesta", chat_id=CHAT_ID, disable_notification=True, protect_content=False, reply_to_message_id=message_id)
            raise(e)
    await session.close()

def get_params(init_message, message, split_size):
    video_len = str(random.randint(10, 60))
    mode = str(random.randint(0, 1))
    gen_photo = str(random.randint(0, 1))
    if message is not None and message.strip() != "":
        init_message = message
    if init_message is not None and init_message.strip() != "":
        init_message = init_message[split_size:].strip()
        splitted_msg = init_message.split("-")
        if len(splitted_msg) == 1 and splitted_msg[0].strip() != '':
            video_len = splitted_msg[0].strip()	
        elif len(splitted_msg) == 2:
            message = splitted_msg[0].strip()
            video_len = splitted_msg[1].strip()
        elif len(splitted_msg) == 3:
            message = splitted_msg[0].strip()
            video_len = splitted_msg[1].strip()
            mode = splitted_msg[2].strip()
        elif len(splitted_msg) == 4:
            message = splitted_msg[0].strip()
            video_len = splitted_msg[1].strip()
            mode = splitted_msg[2].strip()
            gen_photo = splitted_msg[3].strip()

    return "" if message is None else message, video_len, mode, gen_photo


async def genai(update: Update, context: ContextTypes.DEFAULT_TYPE, image=None, video=None):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            
            message, video_len, mode, gen_photo = get_params(update.message.text, None, 7 if (image is None and video is None) else 0)

            if len(mode) == 1 and len(gen_photo) == 1 and (len(video_len) == 1 or len(video_len) == 2):
                if(len(message) <= 500  and not message.endswith('bot')):
                    if get_aivg_online_status():
                        url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/enhance/"+mode+"/"+gen_photo+"/"+video_len+"/"

                        if message is not None and message != "":
                            url = url + urllib.parse.quote(str(message))+"/"
                        await update.message.reply_text("Richiedo una nuova generazione in background", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        asyncio.create_task(ask_for_generation(update.message.message_id, url, image, video))
                    else:
                        await update.message.reply_text("AI API Offline oppure un altra richiesta é ancora in corso, riprova piú tardi", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                else:
                    await update.message.reply_text("se vuoi che genero un video enhanced by LLM il testo deve essere di massimo 500 caratteri", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            else:
                await update.message.reply_text("Errore! Controlla i parametri di input", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
    except (aiohttp.client_exceptions.ServerDisconnectedError, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Generazione interrotta, il server é stato disconnesso", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

application.add_handler(CommandHandler('genai', genai))

async def genpr(update: Update, context: ContextTypes.DEFAULT_TYPE, message=None, image=None, video=None):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            message, video_len, mode, gen_photo = get_params(update.message.text, message, 7 if (image is None and video is None) else 0)
            if len(mode) == 1 and len(gen_photo) == 1 and (len(video_len) == 1 or len(video_len) == 2):
                caption = "Generato utilizzando " + ("FramePack" if mode == "0" else "FramePack F1") + ("" if gen_photo == 0 else ". Immagine iniziale generata con Fooocus") + ". Audio generato tramite MMAudio. Lunghezza video: " + video_len + " secondi."
                if(message is not None and message != "" and len(message) <= 500  and not message.endswith('bot')):
                    if get_aivg_online_status():
                        url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/prompt/"+urllib.parse.quote(str(message))+"/"+mode+"/"+gen_photo+"/"+video_len+"/"
                        await update.message.reply_text("Richiedo una nuova generazione in background", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        asyncio.create_task(ask_for_generation(update.message.message_id, url, image, video))
                    else:
                        await update.message.reply_text("AI API Offline oppure un altra richiesta é ancora in corso, riprova piú tardi", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                else:

                    await update.message.reply_text("se vuoi che genero un video a partire da un prompt devi scrivere una frase dopo /genpr (massimo 500 caratteri)", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            else:
                await update.message.reply_text("Errore! Controlla i parametri di input", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

application.add_handler(CommandHandler('genpr', genpr))

async def genstop(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            application.job_queue.scheduler.pause_job('background_generation')
            await update.message.reply_text("Disabilito la generazione video automatica", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
            #if get_aivg_online_status():
            #    url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/stop/"
            #    connector = aiohttp.TCPConnector(force_close=True)
            #    async with aiohttp.ClientSession(connector=connector) as session:
            #        async with session.get(url,timeout=5) as response:
            #            if (response.status == 200):
            #                await update.message.reply_text("Inviata richiesta per l'interruzione della generazione video attuale", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
            #            else:
            #                await update.message.reply_text(response.reason + " - Si é verificato un errore nella richiesta", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
            #    await session.close()
            #else:
            #    await update.message.reply_text("AI API Offline oppure un altra richiesta é ancora in corso, riprova piú tardi", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

application.add_handler(CommandHandler('genstop', genstop))

async def speak(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
            
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
    
      

          
application.add_handler(CommandHandler('speak', speak))

async def restart(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:

        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
                
            await update.message.reply_text("Riavvio in corso...", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

            os.execl(sys.executable, os.path.abspath(__file__), *sys.argv)
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      

application.add_handler(CommandHandler('restart', restart))

async def generate_from_image(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        message = update.message.caption
        content = await update.message.effective_attachment[-1].get_file()
        if message is not None and message.strip() != "":
            await genpr(update, context, message=message, image=content.file_path)
        else:
            await genai(update, context, image=content.file_path)

    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
application.add_handler(MessageHandler(filters.PHOTO, generate_from_image))

async def generate_from_video(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        message = update.message.caption
        content = await update.message.effective_attachment.get_file()
        if message is not None and message.strip() != "":
            await genpr(update, context, message=message, video=content.file_path)
        else:
            await genai(update, context, video=content.file_path)

    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
        
application.add_handler(MessageHandler(filters.VIDEO, generate_from_video))

async def genloop(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:

        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
                
            await update.message.reply_text("Abilito la generazione video automatica", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            application.job_queue.scheduler.resume_job('background_generation')
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
        

application.add_handler(CommandHandler('genloop', genloop))

async def genimg(update: Update, context: ContextTypes.DEFAULT_TYPE, message=None, image=None):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            message = update.message.text[8:].strip()
            if get_aivg_online_status():
                url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/image/"
                if message is not None and message != "":
                    url = url + urllib.parse.quote(str(message))+"/"
                connector = aiohttp.TCPConnector(force_close=True)
                await update.message.reply_text("Richiedo una nuova generazione", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                async with aiohttp.ClientSession(connector=connector) as session:
                    async with session.post(url,timeout=TIMEOUT) as response:
                        if (response.status == 200):
                            file_path = os.environ.get("TMP_DIR") + "image.png"
                            with open(file_path, "wb") as f:
                                content = await response.content.read()
                                f.write(content)
                            await update.message.reply_photo(file_path, filename=str(uuid.uuid4()) + ".png", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        else:
                            await update.message.reply_text(response.reason + " - Si é verificato un errore nella richiesta", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
                await session.close() 
            else:
                await update.message.reply_text("AI API Offline oppure un altra richiesta é ancora in corso, riprova piú tardi", disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                
    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      

application.add_handler(CommandHandler('genimg', genimg))

async def background_generation():
    try:
        if get_aivg_online_status():
        
            message, video_len, mode, gen_photo = get_params(None, None, 0)
            url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/enhance/"+str(mode)+"/"+str(gen_photo)+"/"+str(video_len)+"/"
            await ask_for_generation(None, url, None, None, from_scheduler=True)
        
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

application.job_queue.scheduler.add_job(lambda: run(background_generation()), trigger='interval', minutes=30, id='background_generation')
#application.job_queue.scheduler.pause_job('background_generation')
application.job_queue.scheduler.resume_job('background_generation')
application.job_queue.scheduler.start()

application.run_polling(allowed_updates=Update.ALL_TYPES)
