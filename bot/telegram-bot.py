import logging
import os
import base64
import random
import requests
import string
import sys
import urllib
import json
import time
import aiohttp
import hashlib
import database
import wave
import asyncio
import uuid
import aiofiles
import re
import moviepy
from pathlib import Path
from asyncio import run
from threading import Thread
from datetime import datetime
from os.path import join, dirname
from dotenv import load_dotenv
from io import BytesIO
from gtts import gTTS
from telegram import InlineKeyboardButton, InlineKeyboardMarkup, ReplyKeyboardMarkup, Update, Bot
from telegram.ext import ApplicationBuilder, CallbackQueryHandler, CommandHandler, ContextTypes, MessageHandler, filters, Defaults
from piper.voice import PiperVoice
from pydub import AudioSegment
from PIL import Image

load_dotenv()

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)


TOKEN = os.environ.get("TOKEN")
CHAT_ID = os.environ.get("CHAT_ID")
GROUP_CHAT_ID = os.environ.get("GROUP_CHAT_ID")
BOT_NAME = os.environ.get("BOT_NAME")
TIMEOUT = 21600
CLEANR = re.compile('<.*?>|&([a-z0-9]+|#[0-9]{1,6}|#x[0-9a-f]{1,6});')

application = ApplicationBuilder().token(TOKEN).connect_timeout(180).defaults(Defaults(block=False)).build()
logging.info("Starting Telegram Client...")

logging.basicConfig(
        format='%(asctime)s %(levelname)-8s %(message)s',
        level=int(os.environ.get("LOG_LEVEL")),
        datefmt='%Y-%m-%d %H:%M:%S')
            
def reply_keyboard():
    return ReplyKeyboardMarkup(
            [["/random", "/ask", "/speak"], ["/genai", "/genpr", "/genimg", "/genloop"],  ["/gencheck", "/genstop", "/restart"]], 
            one_time_keyboard=False
        )

def get_tts_google(text: str):
    mp3_fp = BytesIO()
    tts = gTTS(text=text, lang="it", slow=False)
    tts.write_to_fp(mp3_fp)
    mp3_fp.seek(0)
    return mp3_fp

async def get_aivg_online_status(show_message=True, message_id=None, asking_for_ai=True):
    try:
        r = requests.get(os.environ.get("AIVG_ENDPOINT")+"/aivg/generate/checkrunning/", timeout=5)
        if (r.status_code == 200):
            return True
        elif (asking_for_ai is False and r.status_code == 206):
            return True
        elif (asking_for_ai is True and r.status_code == 206):
            if show_message:
                await Bot(TOKEN).sendMessage(text="Un altra richiesta é ancora in corso, riprova piú tardi", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id)
            return False
        else:
            if show_message:
                await Bot(TOKEN).sendMessage(text="Si é verificato un errore, riprova piú tardi", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id)
            return False
    except (requests.exceptions.ConnectionError, requests.exceptions.Timeout):
        if show_message:
            await Bot(TOKEN).sendMessage(text="AI API Offline, riprova piú tardi", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id)
        logging.info("AIVG Host Offline.")
        return False
    except requests.exceptions.HTTPError:
        if show_message:
            await Bot(TOKEN).sendMessage(text="AI API Offline, riprova piú tardi", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id)
        logging.info("AIVG Host Error 4xx or 5xx.")
        return False
    else:
        return True

async def echo(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            
            message = update.message.text.strip()
            if(message != ""):
                if await get_aivg_online_status(message_id=update.message.message_id):
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
                                await update.message.reply_text(anything_llm_text, reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                            else:
                                logging.error(anything_llm_response)
                                await update.message.reply_text("si è verificato un errore stronzo", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        await anything_llm_session.close()  
                
            else:
                await update.message.reply_text("se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)

    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    

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
                text_found = random.choice(sentences)
                await update.message.reply_audio(get_tts_google(text_found), reply_markup=reply_keyboard(), caption=text_found, disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=str(uuid.uuid4())+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
            else:
                await update.message.reply_text("si è verificato un errore stronzo", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)

    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      

async def ask(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            
            message = update.message.text[5:].strip()
            if(message != "" and len(message) <= 500  and not message.endswith('bot')):
                if await get_aivg_online_status(message_id=update.message.message_id):
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
                                await update.message.reply_audio(get_tts_google(anything_llm_text), reply_markup=reply_keyboard(), caption=anything_llm_text, disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=str(uuid.uuid4())+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
            
                            else:
                                await update.message.reply_text(anything_llm_response.reason + " - Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                        await anything_llm_session.close()  


                
            else:
                await update.message.reply_text("se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico, riprovare in un secondo momento", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      


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
                        time.sleep(1)
                    caption = ""
                    header_items = response.headers.items()
                    reply_markup = None
                    for keyh, valueh in header_items:
                        if keyh.startswith("X-FramePack-") and keyh != "X-FramePack-Prompt" and keyh != "X-FramePack-Prompt-Image":
                            caption = caption + keyh.replace("X-FramePack-","") + ": " + valueh + "\n"
                            if keyh == "X-FramePack-Generation-Id":
                                keyboard = [
                                    [InlineKeyboardButton("Valida", callback_data=("1,"+str(valueh))),InlineKeyboardButton("Skippa", callback_data=("2,"+str(valueh)))],
                                ]
                                reply_markup = InlineKeyboardMarkup(keyboard)
                            #if keyh == "X-FramePack-Prompt":
                            #    prompt = valueh.replace("&nbsp;","\n")
                            #    with open(f'{os.environ.get("TMP_DIR")}prompt.txt', "w") as text_file:
                            #        text_file.write(prompt)
                            #    await Bot(TOKEN).sendDocument(document=open(f'{os.environ.get("TMP_DIR")}prompt.txt', 'rb'), chat_id=CHAT_ID, filename=str(uuid.uuid4())+".mp4", disable_notification=True, protect_content=False, reply_to_message_id=message_id)
                            #if keyh == "X-FramePack-Prompt-Image":
                            #    prompt_image = valueh.replace("&nbsp;","\n")
                            #    with open(f'{os.environ.get("TMP_DIR")}prompt_image.txt', "w") as text_file:
                            #        text_file.write(prompt_image)
                            #    await Bot(TOKEN).sendDocument(document=open(f'{os.environ.get("TMP_DIR")}prompt_image.txt', 'rb'), chat_id=CHAT_ID, filename=str(uuid.uuid4())+".mp4", disable_notification=True, protect_content=False, reply_to_message_id=message_id)
                    await Bot(TOKEN).sendVideo(video=f'{os.environ.get("TMP_DIR")}video.mp4', reply_markup=reply_markup, chat_id=CHAT_ID, filename=str(uuid.uuid4())+".mp4", disable_notification=True, protect_content=False, reply_to_message_id=message_id)
                    await Bot(TOKEN).sendMessage(text=caption, chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id)
                elif response.status == 206 and from_scheduler is False:
                    await Bot(TOKEN).sendMessage(text="Un altra generazione é ancora in corso, riprovare in un secondo momento", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id)
                elif response.status == 408 and from_scheduler is False:
                    await Bot(TOKEN).sendMessage(text=response.reason + " - La generazione del video é stata interrotta perché ha superato il tempo di esecuzione massimo", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id) 
                elif response.status != 206 and response.status != 200 and from_scheduler is False:
                    await Bot(TOKEN).sendMessage(text=response.reason + " - Si é verificato un errore nella richiesta", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id)
                
        except Exception as e:
            if from_scheduler is False:
                await Bot(TOKEN).sendMessage(text=(str(e) + " - Si é verificato un errore nella richiesta"), chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False, reply_to_message_id=message_id)
            raise(e)
    await session.close()

def get_params(init_message, message, split_size):
    video_len = str(7)
    #video_len = str(random.randint(6, 15))
    mode = str(random.randint(0, 1))
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

    return "" if message is None else message, video_len, mode


async def genai(update: Update, context: ContextTypes.DEFAULT_TYPE, image=None, video=None):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            
            message, video_len, mode = get_params(update.message.text, None, 7 if (image is None and video is None) else 0)

            if len(mode) == 1 and (len(video_len) == 1 or len(video_len) == 2):
                if(len(message) <= 500  and not message.endswith('bot')):
                    if await get_aivg_online_status(message_id=update.message.message_id):
                        url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/enhance/"+mode+"/"+video_len+"/"

                        if message is not None and message != "":
                            url = url + urllib.parse.quote(str(message))+"/"
                        await update.message.reply_text("Richiedo una nuova generazione in background", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        asyncio.create_task(ask_for_generation(update.message.message_id, url, image, video))
                else:
                    await update.message.reply_text("se vuoi che genero un video enhanced by LLM il testo deve essere di massimo 500 caratteri", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            else:
                await update.message.reply_text("Errore! Controlla i parametri di input", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except (aiohttp.client_exceptions.ServerDisconnectedError, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Generazione interrotta, il server é stato disconnesso", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)


async def genpr(update: Update, context: ContextTypes.DEFAULT_TYPE, message=None, image=None, video=None):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            message, video_len, mode = get_params(update.message.text, message, 7 if (image is None and video is None) else 0)
            if len(mode) == 1 and (len(video_len) == 1 or len(video_len) == 2):
                if(message is not None and message != "" and len(message) <= 500  and not message.endswith('bot')):
                    if await get_aivg_online_status(message_id=update.message.message_id):
                        url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/prompt/"+urllib.parse.quote(str(message))+"/"+mode+"/"+video_len+"/"
                        await update.message.reply_text("Richiedo una nuova generazione in background", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        asyncio.create_task(ask_for_generation(update.message.message_id, url, image, video))
                else:

                    await update.message.reply_text("se vuoi che genero un video a partire da un prompt devi scrivere una frase dopo /genpr (massimo 500 caratteri)", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            else:
                await update.message.reply_text("Errore! Controlla i parametri di input", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)


async def genstop(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            application.job_queue.scheduler.pause_job('background_generation')
            await update.message.reply_text("Disabilito la generazione video automatica", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)


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
                        await update.message.reply_audio(out, reply_markup=reply_keyboard(), disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=str(uuid.uuid4())+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
                    else:
                        await update.message.reply_text("Voce " + voice_str + " non trovata!", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                else:
                    await update.message.reply_audio(get_tts_google(message), reply_markup=reply_keyboard(), disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=str(uuid.uuid4())+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
                #await embed_message(message)
            else:

                text = "se vuoi che ripeto qualcosa devi scrivere una frase dopo /speak (massimo 500 caratteri)."
                await update.message.reply_text(text, reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
               
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      

          

async def restart(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:

        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
                
            await update.message.reply_text("Riavvio in corso...", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            restart_ai_app()
            os.system("pkill -f python -9")
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      


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
        

async def genloop(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:

        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
                
            await update.message.reply_text("Abilito la generazione video automatica", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            application.job_queue.scheduler.resume_job('background_generation')
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
        


async def genimg(update: Update, context: ContextTypes.DEFAULT_TYPE, message=None, image=None):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            message = update.message.text[8:].strip()
            if await get_aivg_online_status(message_id=update.message.message_id):
                url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/image/"
                if message is not None and message != "":
                    url = url + urllib.parse.quote(str(message))+"/"
                connector = aiohttp.TCPConnector(force_close=True)
                await update.message.reply_text("Richiedo una nuova generazione", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                async with aiohttp.ClientSession(connector=connector) as session:
                    async with session.post(url,timeout=TIMEOUT) as response:
                        if (response.status == 200):
                            file_path = os.environ.get("TMP_DIR") + "image.png"
                            with open(file_path, "wb") as f:
                                content = await response.content.read()
                                f.write(content)
                            await update.message.reply_photo(file_path, filename=str(uuid.uuid4()) + ".png", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        elif response.status == 206:
                            await update.message.reply_text("Un altra generazione é ancora in corso, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                        else:
                            await update.message.reply_text(response.reason + " - Si é verificato un errore nella richiesta", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                await session.close() 
                      
    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)


async def gencheck(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            if await get_aivg_online_status(message_id=update.message.message_id, asking_for_ai=False):
                url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/check/job/"
                connector = aiohttp.TCPConnector(force_close=True)
                async with aiohttp.ClientSession(connector=connector) as session:
                     async with session.post(url,timeout=60) as response:
                        if (response.status >= 200 or response.status < 300):
                            caption = ""
                            if response.status == 200 or response.status == 205:
                                caption = ""
                                file_name = ""
                                generation_id = ""
                                reply_markup = None
                                header_items = response.headers.items()
                                mimetype = None
                                for keyh, valueh in header_items:
                                    if keyh == "X-FramePack-Generation-Id":
                                        generation_id = valueh
                                        keyboard = [
                                            [InlineKeyboardButton("Interrompi", callback_data=("1")),InlineKeyboardButton("Valida", callback_data=("3,"+str(valueh))),InlineKeyboardButton("Skippa", callback_data=("4,"+str(valueh)))],
                                        ]
                                        reply_markup = InlineKeyboardMarkup(keyboard)
                                    elif keyh == "X-FramePack-Check-Current-Job":
                                        caption = valueh.replace("&nbsp;","\n")
                                    elif keyh == "X-FramePack-File-Name" and valueh != "":
                                        file_name = valueh
                                    elif keyh.lower() == "Content-Type".lower():
                                        mimetype = valueh
                                if caption != "":
                                    caption = "File-Name: " + (file_name if file_name != "" else "not available")+ "\nGeneration-Id: " + generation_id + "\n\n" + caption
                                    await update.message.reply_text(caption, reply_to_message_id=update.message.message_id, reply_markup=reply_markup, disable_notification=True, protect_content=False)
                                await update.message.reply_text("Generazione in corso...", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                                if response.status == 200 and mimetype is not None:
                                    filename = os.environ.get("TMP_DIR") + ("video.mp4" if mimetype == "video/mp4" else "image.png")
                                    async with aiofiles.open(filename, 'wb') as f:
                                        async for chunk in response.content.iter_chunked(4096):
                                            await f.write(chunk)
                                        time.sleep(1)
                                    if mimetype == "video/mp4":
                                        await update.message.reply_video(filename, reply_markup=reply_keyboard(), filename=str(uuid.uuid4())+".mp4", disable_notification=True, protect_content=False, reply_to_message_id=update.message.message_id)
                                    elif mimetype == "image/png":
                                        await update.message.reply_photo(filename, filename=str(uuid.uuid4()) + ".png", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                            elif response.status == 201:
                                await update.message.reply_text("Avvio generazione in corso...", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                            elif response.status == 204:
                                await update.message.reply_text("Upscaling in corso...", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                            elif response.status == 206:
                                await update.message.reply_text("Nessuna generazione video in esecuzione", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                            else:
                                await update.message.reply_text(response.reason + " - Si é verificato un errore nella richiesta", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                        else:
                            await update.message.reply_text(response.reason + " - Si é verificato un errore nella richiesta", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                await session.close() 
                      
    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico o potrebbe esserci una generazione ancora in corso, riprovare in un secondo momento", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)


async def set_skipped_status(skipped, generation_id):
    try:
        if await get_aivg_online_status(asking_for_ai=False):
            url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/skipped/" + str(skipped) + "/" + str(generation_id) + "/"
            connector = aiohttp.TCPConnector(force_close=True)
            async with aiohttp.ClientSession(connector=connector) as session:
                async with session.post(url,timeout=TIMEOUT) as response:
                    if (response.status == 200):
                        await Bot(TOKEN).sendMessage(text="Generation-Id #" + str(generation_id) + " impostato a " + ("SKIPPED" if skipped == "2" else "VALID"), chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                    else:
                        await Bot(TOKEN).sendMessage(text=response.reason + " - Si é verificato un errore nella richiesta", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
            await session.close() 
                        
    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await Bot(TOKEN).sendMessage(text="Il server potrebbe essere sovraccarico, riprovare in un secondo momento", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except Exception as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    

async def remove_directory_tree(start_directory: Path):
    """Recursively and permanently removes the specified directory, all of its
    subdirectories, and every file contained in any of those folders."""
    for path in start_directory.iterdir():
        if path.is_file():
            path.unlink()
        else:
            remove_directory_tree(path)

async def background_generation():
    try:
        if await get_aivg_online_status(show_message=False):
                
            message, video_len, mode = get_params(None, None, 0)
            url = os.environ.get("AIVG_ENDPOINT") + "/aivg/generate/enhance/"+str(mode)+"/"+str(video_len)+"/"
            await ask_for_generation(None, url, None, None, from_scheduler=True)
        
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

def restart_ai_app():
    try:
        requests.get(os.environ.get("AIVG_ENDPOINT") + "/aivg/stop/", timeout=5)
    except:
        pass

async def button(update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
    chatid = str(update.effective_chat.id)
    if(CHAT_ID == chatid):
        query = update.callback_query
        await query.answer()
        data = query.data
        splitted_data = data.split(",")
        
        if len(splitted_data) == 1:
            if str(splitted_data[0]) == "1":
                await Bot(TOKEN).sendMessage(text="Interrompo la generazione e riavvio il generatore AI, si prega di attendere prima di inviare un altro comando", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                time.sleep(1)
                restart_ai_app()
        elif len(splitted_data) == 2:
            skipped_status = ""
            if str(splitted_data[0]) == "3":
                skipped_status = "1"
            if str(splitted_data[0]) == "4":
                skipped_status = "2"
            else:
                skipped_status = splitted_data[0]
            await set_skipped_status(skipped_status, splitted_data[1])
            if str(splitted_data[0]) == "3" or str(splitted_data[0]) == "4":
                await Bot(TOKEN).sendMessage(text="Interrompo la generazione e riavvio il generatore AI, si prega di attendere prima di inviare un altro comando", chat_id=CHAT_ID, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                time.sleep(1)
                restart_ai_app()
        else:
            await update.message.reply_text("Errore nell'esecuzione del comando", reply_to_message_id=update.message.message_id, reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)

async def start(update: Update, context: ContextTypes.DEFAULT_TYPE):
    await update.message.reply_text(
        "PezzenteBot avviato",
        reply_markup=reply_keyboard(),
    )

def main() -> None:

    application.add_handler(CommandHandler("start", start))
    application.add_handler(CallbackQueryHandler(button))
    application.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, echo))
    application.add_handler(CommandHandler('random', random_cmd))
    application.add_handler(CommandHandler('ask', ask))
    application.add_handler(CommandHandler('genai', genai))
    application.add_handler(CommandHandler('genpr', genpr))
    application.add_handler(CommandHandler('genstop', genstop))
    application.add_handler(CommandHandler('gencheck', gencheck))
    application.add_handler(CommandHandler('speak', speak))
    application.add_handler(CommandHandler('restart', restart))
    application.add_handler(MessageHandler(filters.PHOTO, generate_from_image))
    application.add_handler(MessageHandler(filters.VIDEO, generate_from_video))
    application.add_handler(CommandHandler('genloop', genloop))
    application.add_handler(CommandHandler('genimg', genimg))

    application.job_queue.scheduler.add_job(lambda: run(background_generation()), trigger='interval', minutes=10, id='background_generation')
    application.job_queue.scheduler.add_job(lambda: run(remove_directory_tree(Path(os.environ.get("TMP_DIR")))), trigger='interval', minutes=120, id='clean_temp_dir')
    #application.job_queue.scheduler.pause_job('background_generation')
    application.job_queue.scheduler.resume_job('background_generation')
    application.job_queue.scheduler.resume_job('clean_temp_dir')
    application.job_queue.scheduler.start()

    application.run_polling(allowed_updates=Update.ALL_TYPES)

if __name__ == "__main__":
    main()
