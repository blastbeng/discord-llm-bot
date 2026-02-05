import logging
import os
import random
import requests
import sys
import aiohttp
import hashlib
import database
import uuid
import aiofiles
import re
import psutil
from pathlib import Path
from asyncio import run
from dotenv import load_dotenv
from io import BytesIO
from gtts import gTTS
from telegram import InlineKeyboardButton, InlineKeyboardMarkup, ReplyKeyboardMarkup, Update, Bot
from telegram.ext import ApplicationBuilder, CallbackQueryHandler, CommandHandler, ContextTypes, MessageHandler, filters, Defaults

load_dotenv()

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)


TOKEN = os.environ.get("TOKEN")
CHAT_ID = os.environ.get("CHAT_ID")
GROUP_CHAT_ID = os.environ.get("GROUP_CHAT_ID")
BOT_NAME = os.environ.get("BOT_NAME")
TIMEOUT = 86400
CLEANR = re.compile('<.*?>|&([a-z0-9]+|#[0-9]{1,6}|#x[0-9a-f]{1,6});')

application = ApplicationBuilder().token(TOKEN).connect_timeout(180).defaults(Defaults(block=False)).build()
logging.info("Starting Telegram Client...")

logging.basicConfig(
        format='%(asctime)s %(levelname)-8s %(message)s',
        level=int(os.environ.get("LOG_LEVEL")),
        datefmt='%Y-%m-%d %H:%M:%S')

class NoRunningFilter(logging.Filter):
    def filter(self, record):
        return not record.msg.startswith('Running job')

#logging.getLogger('apscheduler.executors.default').setLevel(int(os.environ.get("LOG_LEVEL")))
#logging.getLogger("apscheduler.scheduler").addFilter(NoRunningFilter())
            
def reply_keyboard():
    return ReplyKeyboardMarkup(
            [["/random", "/randomai"], ["/speak", "/restart"]], 
            #[["/random", "/ask", "/speak"], ["/story", "/genimg", "/restart"]], 
            one_time_keyboard=False
        )

def get_tts_google(text: str):
    mp3_fp = BytesIO()
    tts = gTTS(text=text, lang="it", slow=False)
    tts.write_to_fp(mp3_fp)
    mp3_fp.seek(0)
    return mp3_fp

async def echo(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):
            
            message = update.message.text.strip()
            #cpu_percent = psutil.cpu_percent()
            #if int(cpu_percent) > 70:                
            #    cpu_message = "Il server é sovraccarico, riprovare fra qualche istante"
            #    cpu_message = cpu_message + "\n"
            #    cpu_message = cpu_message + "*CPU: " + str(cpu_percent) + "% - RAM: " + str(psutil.virtual_memory()[2]) + "%*"
            #    await update.message.reply_text(cpu_message, reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            #el
            if(message != "" and len(message) <= 500):
                data = {
                        "message": message.rstrip(),
                        "mode": "chat",
                        "reset": "true"
                }
                headers = {
                    'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
                }
                anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
                connector = aiohttp.TCPConnector(force_close=True)
                session_timeout = aiohttp.ClientTimeout(total=None,sock_connect=900,sock_read=900)
                async with aiohttp.ClientSession(connector=connector, timeout=session_timeout) as anything_llm_session:
                    async with anything_llm_session.post(anything_llm_url, headers=headers, json=data, timeout=900) as anything_llm_response:
                        if (anything_llm_response.status == 200):
                            anything_llm_json = await anything_llm_response.json()
                            #anything_llm_text = anything_llm_json["textResponse"].partition('\n')[0].lstrip('\"').rstrip('\"').rstrip()
                            #anything_llm_text = anything_llm_json["textResponse"].replace("\n", " ").replace("\r", " ")
                            anything_llm_text = anything_llm_json["textResponse"]
          #                  await update.message.reply_audio(get_tts_google(anything_llm_text), reply_markup=reply_keyboard(), caption=anything_llm_text, disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=str(uuid.uuid4())+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
                            await update.message.reply_text(anything_llm_text, reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)

                        elif (anything_llm_response.status == 503):
                            await update.message.reply_text(anything_llm_response.reason + "\n\nUn'altra richiesta é ancora in esecuzione.\nRiprovare in un secondo momento.\nNOTA: Questo server gestisce una richiesta per volta.", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                        else:
                            logging.error(anything_llm_response.reason)
                            await update.message.reply_text(anything_llm_response.reason + "\n\nIl server IA é spento. Riprovare in un secondo momento.", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                
                    await anything_llm_session.close()  
            
            else:
                await update.message.reply_text("se vuoi dirmi o chiedermi qualcosa devi scrivere una frase dopo /ask (massimo 500 caratteri)", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)

    except Exception:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)



def compute_md5_hash(my_string):
    m = hashlib.md5()
    m.update(my_string.encode('utf-8'))
    return m.hexdigest()


async def embed_message(text):
    try:
        data = {
            "textContent": text,
            "addToWorkspaces": os.environ.get("ANYTHING_LLM_WORKSPACE"),
            "metadata": {
                "title": "sentences_" + str(compute_md5_hash(text))
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

    except Exception:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      
async def random_ai(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:
        chatid = str(update.effective_chat.id)
        if(CHAT_ID == chatid):#
            message = random.choice(database.select_all_sentence(dbms))
            init_message = await update.message.reply_text(message, reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
                        
            
            connector = aiohttp.TCPConnector(force_close=True)
            anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
            data = {
                    "message": message.rstrip(),
                    "mode": "chat",
                    "reset": "true"
            }
            headers = {
                'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
            }
            session_timeout = aiohttp.ClientTimeout(total=None,sock_connect=900,sock_read=900)
            async with aiohttp.ClientSession(connector=connector, timeout=session_timeout) as anything_llm_session:
                async with anything_llm_session.post(anything_llm_url, headers=headers, json=data, timeout=900) as anything_llm_response:
                    if (anything_llm_response.status == 200):
                        anything_llm_json = await anything_llm_response.json()
                        #anything_llm_text = anything_llm_json["textResponse"].partition('\n')[0].lstrip('\"').rstrip('\"').rstrip()
                        #anything_llm_text = anything_llm_json["textResponse"].replace("\n", " ").replace("\r", " ")
                        anything_llm_text = anything_llm_json["textResponse"]
                        await update.message.reply_text(anything_llm_text, reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=init_message.message_id, protect_content=False)

#                        await update.message.reply_audio(get_tts_google(anything_llm_text), reply_markup=reply_keyboard(), caption=anything_llm_text, disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=str(uuid.uuid4())+ "audio.mp3", reply_to_message_id=init_message.message_id, protect_content=False)
                  
                    else:
                        logging.error(anything_llm_response.reason)
                        await update.message.reply_text(anything_llm_response.reason + "\n\nIl server IA potrebbe essere offline oppure potrebbero esserci altre richieste ancora in corso. Riprovare in un secondo momento.", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
                 
            await anything_llm_session.close()  

    except (requests.exceptions.RequestException, ValueError) as e:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
      await update.message.reply_text("Il server potrebbe essere sovraccarico, riprovare in un secondo momento", reply_markup=reply_keyboard(), disable_notification=True, protect_content=False)
    except Exception:
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
            if(message != "" and len(message) <= 500):
                await update.message.reply_audio(get_tts_google(message), reply_markup=reply_keyboard(), disable_notification=True, title="Messaggio vocale", performer="Pezzente",  filename=str(uuid.uuid4())+ "audio.mp3", reply_to_message_id=update.message.message_id, protect_content=False)
                await embed_message(message)
            else:

                text = "se vuoi che ripeto qualcosa devi scrivere una frase dopo /speak (massimo 500 caratteri)."
                await update.message.reply_text(text, reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
               
    except Exception:
      exc_type, exc_obj, exc_tb = sys.exc_info()
      fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
      logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    
      

          

async def restart(update: Update, context: ContextTypes.DEFAULT_TYPE):
    try:

        chatid = str(update.effective_chat.id)
        if((CHAT_ID == chatid or GROUP_CHAT_ID == chatid)):
                
            await update.message.reply_text("Riavvio in corso...", reply_markup=reply_keyboard(), disable_notification=True, reply_to_message_id=update.message.message_id, protect_content=False)
            #restart_ai_app()
            os.system("pkill -f python -9")
    except Exception:
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

async def start(update: Update, context: ContextTypes.DEFAULT_TYPE):
    await update.message.reply_text(
        "PezzenteBot avviato",
        reply_markup=reply_keyboard(),
    )

def main() -> None:

    application.add_handler(CommandHandler("start", start))
    application.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, echo))
    application.add_handler(CommandHandler('random', random_cmd))
    application.add_handler(CommandHandler('randomai', random_ai))
    application.add_handler(CommandHandler('speak', speak))
    application.add_handler(CommandHandler('restart', restart))
    application.job_queue.scheduler.add_job(lambda: run(remove_directory_tree(Path(os.environ.get("TMP_DIR")))), trigger='interval', minutes=30, id='clean_temp_dir')
    application.job_queue.scheduler.resume_job('clean_temp_dir')
    application.job_queue.scheduler.start()

    application.run_polling(allowed_updates=Update.ALL_TYPES)

if __name__ == "__main__":
    main()
