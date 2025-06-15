import os
import sys
import json
import time
import utils
import database
import socket
import random as randompy
import urllib
import logging
import pathlib
import urllib.request
from PIL import Image
from typing import Optional
from os.path import join as joinpy
from os.path import dirname
from dotenv import load_dotenv, set_key
import discord
from discord import app_commands
from discord.ext import commands, tasks
from discord.errors import ClientException
from datetime import datetime
from typing import List
import asyncio
import requests
import requests_cache
import aiohttp
import io
from random import randint
import requests.exceptions
import psutil
import json
from io import BytesIO
from gtts import gTTS
from utils import FFmpegPCMAudioBytesIO
from datetime import timedelta
import uuid
from piper.voice import PiperVoice
from pydub import AudioSegment
import wave
import copy

dotenv_path = joinpy(dirname(__file__), '.env')
load_dotenv(dotenv_path)


requests_cache.install_cache('discord_bot_cache')

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)

GUILD_ID = discord.Object(id=os.environ.get("GUILD_ID"))


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

def get_tts_google(text: str):
    mp3_fp = BytesIO()
    tts = gTTS(text=text, lang="it", slow=False)
    tts.write_to_fp(mp3_fp)
    mp3_fp.seek(0)
    return mp3_fp 



def get_tts_piper(text: str, voice_str: str):
    model = joinpy(dirname(__file__), "models/" + voice_str + '.onnx') 
    if os.path.isfile(model):
        voice = PiperVoice.load(model)
        file_path = os.environ.get("TMP_DIR") + str(uuid.uuid4()) + ".wav"
        with wave.open(file_path, "w") as wav_file:
            voice.synthesize(text, wav_file)
        audio = AudioSegment.from_wav(file_path)
        out = BytesIO()
        audio.export(out, format='mp3', bitrate="256k")
        out.seek(0)
        os.remove(file_path)
        return out
    return None

class MyClient(discord.Client):
    def __init__(self, *, intents: discord.Intents):
        super().__init__(intents=intents)
        self.tree = app_commands.CommandTree(self)

class AdminPermissionError(Exception):
    pass

class ExcludedPermissionError(Exception):
    pass

class PermissionError(Exception):
    pass

class NoChannelError(Exception):
    pass

class CustomTextInput(discord.ui.TextInput):
        
    def __init__(self, style, name):
        super().__init__(style=style, label=name, value="")

        
class PlayButton(discord.ui.Button["InteractionRoles"]):

    def __init__(self, message, content):
        super().__init__(style=discord.ButtonStyle.green, label="Play")
        self.message = message
        self.content = content
    
    async def callback(self, interaction: discord.Interaction):
        is_deferred=True
        try:
            await interaction.response.defer(thinking=True, ephemeral=True)
                    
            check_permissions(interaction)

            voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
            await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)
            if voice_client:
                if not voice_client.is_connected():
                    await voice_client.channel.connect()
                    time.sleep(5)

                if voice_client is not None and voice_client.is_playing():
                    await voice_client.stop()

            if voice_client is not None:
                voice_client.play(FFmpegPCMAudioBytesIO(copy.deepcopy(self.content).read(), pipe=True), after=lambda e: logging.info("play_button - " + self.message))
                await interaction.followup.send(self.message, ephemeral = True)
                
        except Exception as e:
            await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)
            
class StopButton(discord.ui.Button["InteractionRoles"]):

    def __init__(self):
        super().__init__(style=discord.ButtonStyle.red, label="Stop")
    
    async def callback(self, interaction: discord.Interaction):
        is_deferred=True
        try:
            await interaction.response.defer(thinking=True, ephemeral=True)
            check_permissions(interaction)
            voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
            await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)

            logging.info("stop - StopButton.callback.stop()")
            await interaction.followup.send("Interrompo il bot", ephemeral = True)
            voice_client.stop()
                
        except Exception as e:
            await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

intents = discord.Intents.all()
client = MyClient(intents=intents)


logging.info("Starting Discord Client...")

logging.basicConfig(
        format='%(asctime)s %(levelname)-8s %(message)s',
        level=int(os.environ.get("LOG_LEVEL")),
        datefmt='%Y-%m-%d %H:%M:%S')

logging.getLogger('discord').setLevel(int(os.environ.get("LOG_LEVEL")))
logging.getLogger('discord.client').setLevel(int(os.environ.get("LOG_LEVEL")))
logging.getLogger('discord.gateway').setLevel(int(os.environ.get("LOG_LEVEL")))
logging.getLogger('discord.voice_client').setLevel(int(os.environ.get("LOG_LEVEL")))

discord.utils.setup_logging(level=int(os.environ.get("LOG_LEVEL")), root=False)

def get_available_voices():
    voices = []
    filenames = next(os.walk(joinpy(dirname(__file__), "models")), (None, None, []))[2]
    for filename in filenames:
        name = filename.split(".")[0]
        if name not in voices:
            voices.append(name)
    voices.append("Google")
    return voices

available_voices = get_available_voices()

async def rps_autocomplete(interaction: discord.Interaction, current: str) -> List[app_commands.Choice[str]]:
    global available_voices
    available_voices = get_available_voices()
    choices = {}
    for voice in available_voices:
        choices [voice] = voice
    choices ["random"] = "random"
    choices = [app_commands.Choice(name=choice, value=choice) for choice in choices if current.lower() in choice.lower()][:25]
    return choices

audio_count_queue = 0

async def send_error(e, interaction, from_generic=False, is_deferred=False):
    logging.error(e)
    currentguildid=get_current_guild_id(interaction.guild.id)
    if isinstance(e, app_commands.CommandOnCooldown):
        try:
            dtc = "Spam detected."
            spamarray=[]
            spamarray.append(dtc + " " + interaction.user.mention + " Ti sto guardando.")
            spamarray.append(dtc + " " + interaction.user.mention + " Questo non ti rende una brava persona.")
            spamarray.append(dtc + " " + interaction.user.mention + " Sono stupido ma non noioso.")
            spamarray.append(dtc + " " + interaction.user.mention + " Prenditi il tuo tempo.")
            spamarray.append(dtc + " " + interaction.user.mention + " Mantieni la calma.")
            spamarray.append(dtc + " " + interaction.user.mention + " Anche a casa tua ti comporto cosí?")
            spamarray.append(dtc + " " + interaction.user.mention + " Perché sei cosí ansioso?")
            spamarray.append(dtc + " " + interaction.user.mention + " Ti aggiungo alla blacklist.")
            command = str(interaction.data['name'])
            cooldown = command + ' -> Cooldown: ' + str(e.cooldown.per) + '[' + str(round(e.retry_after, 2)) + ']s'

            spaminteractionmsg = utils.get_random_from_array(spamarray) + '\n' + cooldown
            if is_deferred:
                await interaction.followup.send(spaminteractionmsg, ephemeral = True)
            else:
                await interaction.response.send_message(spaminteractionmsg, ephemeral = True)
        except Exception as e:
            exc_type, exc_obj, exc_tb = sys.exc_info()
            fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
            logging.error("[GUILDID : %s] %s %s %s", currentguildid, exc_type, fname, exc_tb.tb_lineno, exc_info=1)
            if is_deferred:
                await interaction.followup.send("Discord API Error, per favore riprova piú tardi", ephemeral = True)
            else:
                await interaction.response.send_message("Discord API Error, per favore riprova piú tardi", ephemeral = True)
    elif isinstance(e, ExcludedPermissionError):
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("[GUILDID : %s] %s %s %s - %s", currentguildid, exc_type, fname, exc_tb.tb_lineno, e.args[0])
        
        voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
        await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)
        if is_deferred:
            await interaction.followup.send("Disagio.", ephemeral = True)
        else:
            await interaction.response.send_message("Disagio.", ephemeral = True)
    elif isinstance(e, AdminPermissionError):
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("[GUILDID : %s] %s %s %s - %s", currentguildid, exc_type, fname, exc_tb.tb_lineno, e.args[0])
        if is_deferred:
            await interaction.followup.send("Non hai i permessi per utilizzare questo comando.", ephemeral = True)
        else:
            await interaction.response.send_message("Non hai i permessi per utilizzare questo comando.", ephemeral = True)
    elif isinstance(e, PermissionError):
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("[GUILDID : %s] %s %s %s - %s", currentguildid, exc_type, fname, exc_tb.tb_lineno, e.args[0])
        if is_deferred:
            await interaction.followup.send("Non hai i permessi per utilizzare questo comando in questo canale vocale.", ephemeral = True)
        else:
            await interaction.response.send_message("Non hai i permessi per utilizzare questo comando in questo canale vocale.", ephemeral = True)
    elif isinstance(e, NoChannelError):
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("[GUILDID : %s] %s %s %s - %s", currentguildid, exc_type, fname, exc_tb.tb_lineno, e.args[0])
        if is_deferred:
            await interaction.followup.send("Devi essere connesso a un canale vocale per utilizzare questo comando", ephemeral = True)
        else:
            await interaction.response.send_message("Devi essere connesso a un canale vocale per utilizzare questo comando", ephemeral = True)
    else:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("[GUILDID : %s] %s %s %s", currentguildid, exc_type, fname, exc_tb.tb_lineno, e.args[0])
        if is_deferred:
            await interaction.followup.send("Discord API Error, per favore riprova piú tardi", ephemeral = True)
        else:
            await interaction.response.send_message("Discord API Error, per favore riprova piú tardi", ephemeral = True)
    

def get_voice_client_by_guildid(voice_clients, guildid):
    for vc in voice_clients:
        if vc.guild.id == guildid:
            return vc
    return None

def check_permissions(interaction):
    if not interaction.user.voice or not interaction.user.voice.channel:
        raise NoChannelError("NO CHANNEL ERROR - User [ NAME: " + str(interaction.user.name) + " - ID: " + str(interaction.user.id) + "] tried to use a command without being connected to a voice channel")

    perms = interaction.user.voice.channel.permissions_for(interaction.user.voice.channel.guild.me)
    if (not perms.speak):
        raise PermissionError("PERMISSION ERROR - User [NAME: " + str(interaction.user.name) + " - ID: " + str(interaction.user.id) + "] some excluded user tried to use a command")

def check_admin_permissions(interaction):
    if (not str(interaction.user.id) == str(os.environ.get("ADMIN_ID"))):
        raise AdminPermissionError("ADMIN PERMISSION ERROR - User [NAME: " + str(interaction.user.name) + " - ID: " + str(interaction.user.id) + "] tried to use a command who requires admin grants")
        

async def connect_bot_by_voice_client(voice_client, channel, guild, member=None):
    try:  
        if (voice_client and not voice_client.is_playing() and voice_client.channel and voice_client.channel.id != channel.id) or (not voice_client or not voice_client.channel):
            if member is not None and member.id is not None:
                if voice_client and voice_client.channel:
                    for memberSearch in voice_client.channel.members:
                        if member.id == memberSearch.id:
                            channel = voice_client.channel
                            break
            perms = channel.permissions_for(channel.guild.me)
            if (perms.administrator or perms.speak):
                if voice_client and voice_client.channel and voice_client.is_connected():
                    await voice_client.disconnect()
                    time.sleep(5)
                await channel.connect()
    except TimeoutError as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    except ClientException as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

def get_current_guild_id(guildid):
    if str(guildid) == str(os.environ.get("GUILD_ID")):
        return '000000' 
    else:
        return str(guildid)

class PlayAudioWorker:
    
    def __init__(self, text, interaction, message, voice, save=False, initial_text=None):
        global audio_count_queue
        audio_count_queue = audio_count_queue + 1
        self.interaction = interaction
        self.text = text
        self.message = message
        self.voice = voice
        self.save = save
        self.initial_text = initial_text

    @tasks.loop(seconds=0.1, count=1)
    async def play_audio_worker(self):
        global audio_count_queue
        currentguildid = get_current_guild_id(str(self.interaction.guild.id))
        try:
            content = None
            if self.voice == "Google":
                content = get_tts_google(self.text)
            elif self.voice == "random":
                rnd_voice = randompy.choice(available_voices)
                if rnd_voice == "Google":
                    content = get_tts_google(self.text)
                else:
                    content = get_tts_piper(self.text, rnd_voice)
            else:
                content = get_tts_piper(self.text, self.voice)
            voice_client = get_voice_client_by_guildid(client.voice_clients, self.interaction.guild.id)            
            if not voice_client:
                raise ClientException("voice_client is None")
            if voice_client.is_playing():
                voice_client.stop()               
                            
            if not voice_client.is_connected():
                await voice_client.channel.connect()
                time.sleep(5)   

            if content is not None:                

                view = discord.ui.View()
                view.add_item(PlayButton(self.text, copy.deepcopy(content)))
                view.add_item(StopButton())
                logmessage = 'play_audio_worker - ' + self.text
                voice_client.play(FFmpegPCMAudioBytesIO(copy.deepcopy(content).read(), pipe=True), after=lambda e: logging.info(logmessage))
                await self.interaction.followup.edit_message(message_id=self.message.id,content=self.text if self.initial_text is None else self.initial_text + self.text, view = view)
            else:
                await self.interaction.followup.edit_message(message_id=self.message.id,content="Errore nella generazione dell'audio, prova a selezionare una voce diversa.")

            if self.save:
                database.insert_sentence(dbms, self.text)

        except Exception as e:
            exc_type, exc_obj, exc_tb = sys.exc_info()
            fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
            logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
            logging.error("[GUILDID : %s] play_audio_worker - Received bad response from APIs.", str(get_current_guild_id(self.interaction.guild.id)))
            raise Exception("play_audio_worker - Error! - ")
            
        if audio_count_queue > 0:
            audio_count_queue = audio_count_queue - 1

async def get_queue_message():
    global audio_count_queue
    message = "\n\n"
    message = message + "Se il server é sovraccarico, potrebbe volerci un po' di tempo"
    message = message + "\n"
    message = message + "*CPU: " + str(psutil.cpu_percent()) + "% - RAM: " + str(psutil.virtual_memory()[2]) + "%*"
    message = message + "\n"
    message = message + "**" + "TTS in coda:" + " " + str(0 if audio_count_queue == 0 else audio_count_queue - 1) + "**"
    return message

@tasks.loop(hours=6)
async def change_presence_loop():
    try:
        url = "https://steamspy.com/api.php?request=top100in2weeks"
        connector = aiohttp.TCPConnector(force_close=True)
        async with aiohttp.ClientSession(connector=connector) as session:
            async with session.get(url) as response:
                if (response.status == 200):                
                    text = await response.text()
                    json_games = json.loads(text)
                    game_array = []
                    for key, value in json_games.items():
                        game_array.append(value['name'])
                    game = str(utils.get_random_from_array(game_array))
                    logging.info("change_presence_loop - change_presence - game: " + game)
                    await client.change_presence(activity=discord.Game(name=game))
                else:
                    logging.error("change_presence_loop - steamspy API ERROR - status_code: " + str(response.status))
            await session.close()  
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)


@client.event
async def on_ready():
    try:
        logging.info(f'Logged in as {client.user} (ID: {client.user.id})')
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

@client.event
async def on_connect():
    try:
        logging.info(f'Connected as {client.user} (ID: {client.user.id})')
        if not change_presence_loop.is_running():
            change_presence_loop.start()

    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

@client.event
async def on_guild_available(guild):
    try:
        currentguildid = get_current_guild_id(str(guild.id))

    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

    try:
        client.tree.copy_global_to(guild=guild)
        await client.tree.sync(guild=guild)
        logging.info(f'Syncing commands to Guild (ID: {guild.id}) (NAME: {guild.name})')
    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

@client.event
async def on_guild_join(guild):
    logging.info(f'Guild joined (ID: {guild.id})')
    client.tree.copy_global_to(guild=guild)
    await client.tree.sync(guild=guild)
    logging.info(f'Syncing commands to Guild (ID: {guild.id}) (NAME: {guild.name})')

@client.tree.command()
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
async def join(interaction: discord.Interaction):
    """Join channel."""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)
        check_permissions(interaction)
        
        voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
        if voice_client:       
            await voice_client.disconnect()
            time.sleep(5)
        await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)

        await interaction.followup.send("Sto entrando nel canale", ephemeral = True)
         
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@client.tree.command()
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
async def leave(interaction: discord.Interaction):
    """Leave channel"""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)
        check_permissions(interaction)
        
        voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
        if voice_client:       
            await voice_client.disconnect()     
            await interaction.followup.send("Sto lasciando il canale", ephemeral = True)
        else:
            await interaction.followup.send("Non sono connesso a nessun canale", ephemeral = True)       
         
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@client.tree.command()
@app_commands.rename(text='text')
@app_commands.describe(text="La frase da ripetere")
@app_commands.rename(voice='voice')
@app_commands.describe(voice="La voce da usare")
@app_commands.autocomplete(voice=rps_autocomplete)
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
async def speak(interaction: discord.Interaction, text: str, voice: str = "Google"):
    """Repeat a sentence"""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)
        check_permissions(interaction)
        
        voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
        await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)

        if voice_client and not hasattr(voice_client, 'play') and voice_client.is_connected():
            await interaction.followup.send("Per favore riprova piú tardi, Sto inizializzando la connessione...", ephemeral = True)
        else:

            currentguildid = get_current_guild_id(interaction.guild.id)
            message:discord.Message = await interaction.followup.send("Inizio a generare l'audio per la frase:" + " **" + text + "**" + await get_queue_message(), ephemeral = True)
            worker = PlayAudioWorker(text, interaction, message, voice, save=True)
            worker.play_audio_worker.start()
                 
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@client.tree.command()
@app_commands.rename(audio='audio')
@app_commands.describe(audio="Il file audio (mp3 or wav)")
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
async def audio(interaction: discord.Interaction, audio: discord.Attachment):
    """Audio playback from the input audio"""
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)        
        check_permissions(interaction)
        
        voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
        await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)

        if voice_client and not hasattr(voice_client, 'play') and voice_client.is_connected():
            await interaction.followup.send("Per favore riprova piú tardi, Sto inizializzando la connessione...", ephemeral = True)
        elif voice_client:            
            if not utils.allowed_audio(audio.filename):
                await interaction.followup.send("The file extension is not valid.", ephemeral = True)     
            else:
                audiofile = await audio.to_file()            
                voice_client.play(FFmpegPCMAudioBytesIO(audiofile.fp.read(), pipe=True), after=lambda e: logging.info(urllib.parse.quote(str(interaction.user.name)) + " requested an audio playback from file"))
                await interaction.followup.send("Done! I'm starting the audio playback!", ephemeral = True)   
        else:
            await interaction.followup.send("Il bot non é ancora pronto opppure un altro user sta usando qualche altro comando.\nPer favore riprova piú tardi o utilizza il comando /stop", ephemeral = True)            
        
    except Exception as e:
        await send_error(e, interaction)

@client.tree.command()
@app_commands.rename(text='text')
@app_commands.describe(text="La frase da chiedere")
@app_commands.rename(voice='voice')
@app_commands.describe(voice="La voce da usare")
@app_commands.autocomplete(voice=rps_autocomplete)
@app_commands.checks.cooldown(1, 30.0, key=lambda i: (i.user.id))
async def ask(interaction: discord.Interaction, text: str, voice: str = "Google"):
    """Ask something."""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=False)
        check_permissions(interaction)
        
        voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
        await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)

        
        if voice_client and not hasattr(voice_client, 'play') and voice_client.is_connected():
            await interaction.followup.send("Per favore riprova piú tardi, Sto inizializzando la connessione...", ephemeral = True)
        else:
            currentguildid = get_current_guild_id(interaction.guild.id)
            if currentguildid == '000000' and get_anythingllm_online_status():
                data = {
                        "message": text.rstrip(),
                        "mode": "chat"
                    }
                headers = {
                    'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
                }
                connector = aiohttp.TCPConnector(force_close=True)
                anything_llm_url = os.environ.get("ANYTHING_LLM_ENDPOINT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/chat"
                message:discord.Message = await interaction.followup.send('**' + str(interaction.user.name) + "** ha chiesto al bot:" + " **" + text + "**" + await get_queue_message(), ephemeral = False)            
                async with aiohttp.ClientSession(connector=connector) as anything_llm_session:
                    async with anything_llm_session.post(anything_llm_url, headers=headers, json=data) as anything_llm_response:
                        if (anything_llm_response.status == 200):
                            anything_llm_json = await anything_llm_response.json()
                            anything_llm_text = anything_llm_json["textResponse"].partition('\n')[0].lstrip('\"').rstrip('\"').rstrip()
                            
                            worker = PlayAudioWorker(anything_llm_text, interaction, message, voice, initial_text="**"+str(interaction.user.name) + '**: '+ text + '\n**' + interaction.guild.me.nick + "**: ")
                            worker.play_audio_worker.start()
                    await anything_llm_session.close()
            else:
                await interaction.followup.send("Il Chatbot AI é offline, per favore riprova piú tardi", ephemeral = True) 
                   
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@client.tree.command()
@app_commands.rename(voice='voice')
@app_commands.describe(voice="La voce da usare")
@app_commands.autocomplete(voice=rps_autocomplete)
@app_commands.rename(text='text')
@app_commands.describe(text="Il testo da cercare")
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
async def random(interaction: discord.Interaction, voice: str = "Google", text: str = ""):
    """Say a random sentence"""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)
        check_permissions(interaction)
        
        voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
        await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)

        if voice_client and (not hasattr(voice_client, 'play') or not voice_client.is_connected()):
            await interaction.followup.send("Per favore riprova piú tardi, sto initializzando la connessione...", ephemeral = True)
        elif voice_client:
            
            currentguildid = get_current_guild_id(interaction.guild.id)  
                
            sentences = None

            if text is not None:
                sentences = database.select_like_sentence(dbms, text)
            else:
                sentences = database.select_all_sentence(dbms)

            if sentences is not None and len(sentences) > 0:
                message:discord.Message = await interaction.followup.send("Sto cercando una frase casuale"  + await get_queue_message(), ephemeral = True)
                
                worker = PlayAudioWorker(randompy.choice(sentences), interaction, message, voice)
                worker.play_audio_worker.start()
            else:
                await interaction.followup.send((('Nessuna frase trovata contenente il testo "'+text+'"') if text is not None else "") + "Nothing found", ephemeral = True)

        else:
            await interaction.followup.send("Il bot non é ancora pronto opppure un altro user sta usando qualche altro comando.\nPer favore riprova piú tardi o utilizza il comando /stop", ephemeral = True)

         
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@client.tree.command()
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
@app_commands.guilds(discord.Object(id=os.environ.get("GUILD_ID")))
async def restart(interaction: discord.Interaction):
    """Restart bot."""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)
        if str(interaction.guild.id) == str(os.environ.get("GUILD_ID")) and str(interaction.user.id) == str(os.environ.get("ADMIN_ID")):
            if interaction.user.guild_permissions.administrator:
                await interaction.followup.send("Sto riavviando il bot.", ephemeral = True)
                os.execv(sys.executable, ['python'] + sys.argv)
            else:
                await interaction.followup.send("Solo gli amministratori possono utilizzare questo comando", ephemeral = True)
        else:
            await interaction.followup.send("Solo gli amministratori possono utilizzare questo comando nel server padre", ephemeral = True)
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@client.tree.command()
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
async def stop(interaction: discord.Interaction):
    """Stop playback."""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)
        check_permissions(interaction)
        voice_client = get_voice_client_by_guildid(client.voice_clients, interaction.guild.id)
        await connect_bot_by_voice_client(voice_client, interaction.user.voice.channel, interaction.guild)

        logging.info("stop - voice_client.stop()")
        await interaction.followup.send("Interrompo il bot", ephemeral = True)
        voice_client.stop()
            
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@client.tree.command()
@app_commands.rename(name='name')
@app_commands.describe(name="Nuovo nickname del bot (limite di 32 caratteri)")
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
async def rename(interaction: discord.Interaction, name: str):
    """Rename bot."""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)
        #check_permissions(interaction)
        
        if len(name) < 32:
            currentguildid = get_current_guild_id(interaction.guild.id)
            
            message = "Mi hai rinominato in" + ' "'+name+'"'
            await interaction.guild.me.edit(nick=name)
            await interaction.followup.send(message, ephemeral = True)
        else:
            await interaction.followup.send("Il mio nickname non puó essere piú lungo di 32 caratteri", ephemeral = True)
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@client.tree.command()
@app_commands.guilds(discord.Object(id=os.environ.get("GUILD_ID")))
@app_commands.rename(image='image')
@app_commands.describe(image="Nuovo avatar del bot")
@app_commands.checks.cooldown(1, 5.0, key=lambda i: (i.user.id))
async def avatar(interaction: discord.Interaction, image: discord.Attachment):
    """Change bot avatar."""
    is_deferred=True
    try:
        await interaction.response.defer(thinking=True, ephemeral=True)
        #check_permissions(interaction)
        if str(interaction.guild.id) == str(os.environ.get("GUILD_ID")):
            imgfile=await image.to_file()
            filepath = os.environ.get("TMP_DIR") + "/avatar_guild_" + str(interaction.guild.id) + pathlib.Path(imgfile.filename).suffix
            with open(filepath, 'wb') as file:
                file.write(imgfile.fp.getbuffer())
            if check_image_with_pil(filepath):
                with open(filepath, 'rb') as f:
                    image = f.read()
                await client.user.edit(avatar=image)
                await interaction.followup.send("L'immagine é stata modificata", ephemeral = True)
                os.remove(filepath)
            else:
                await interaction.followup.send("Questo tipo di file non é supportato", ephemeral = True)
        else:
            await interaction.followup.send("Solo gli amministratori possono utilizzare questo comando nel server padre", ephemeral = True)
    except Exception as e:
        await send_error(e, interaction, from_generic=False, is_deferred=is_deferred)

@audio.error
@avatar.error
@join.error
@leave.error
@random.error
@rename.error
@speak.error
@stop.error
async def on_generic_error(interaction: discord.Interaction, e: app_commands.AppCommandError):
    await send_error(e, interaction, from_generic=True)

client.run(os.environ.get("BOT_TOKEN"))
