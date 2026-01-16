
import database
import aiohttp
import logging
import os
import asyncio
import sys
import time
import hashlib
import time
from tqdm import tqdm
from os.path import join, dirname
from dotenv import load_dotenv

dotenv_path = join(dirname(__file__), '.env')
load_dotenv(dotenv_path)

async def upload_message(text, title):
    document = None
    data = {
        "textContent": text,
        "metadata": {
            "title": title
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
            if (anything_llm_response.status == 200):
                anything_llm_json = await anything_llm_response.json()
                document = anything_llm_json["documents"][0]["location"]
            else:
                logging.error(anything_llm_response)
                with open(join(dirname(__file__), 'config/' + title + ".txt"), 'w') as text_file:
                    text_file.write(text)
        await anything_llm_session.close()  
    return document

async def embed_message(anything_llm_document):
    data_embed = {
        "adds": [ anything_llm_document ]
    }
    headers = {
        'Authorization': 'Bearer ' + os.environ.get("ANYTHING_LLM_API_KEY")
    }
    connector = aiohttp.TCPConnector()
    session_timeout = aiohttp.ClientTimeout(total=None,sock_connect=900,sock_read=900)
    async with aiohttp.ClientSession(connector=connector, timeout=session_timeout) as anything_llm_session:
        anything_llm_url_embed = os.environ.get("ANYTHING_LLM_ENDPOINT_NO_LIMIT") + "/api/v1/workspace/" + os.environ.get("ANYTHING_LLM_WORKSPACE") + "/update-embeddings"
        async with anything_llm_session.post(anything_llm_url_embed, headers=headers, json=data_embed, timeout=900) as anything_llm_response_embed:
            if (anything_llm_response_embed.status != 200):
                logging.error(anything_llm_response_embed)
                
        await anything_llm_session.close()  

def split(a, n):
    k, m = divmod(len(a), n)
    return (a[i*k+min(i, m):(i+1)*k+min(i+1, m)] for i in range(n))

async def start(filepath):
    try:
        with open(filepath, 'rt') as f:
            data = f.readlines()
            documents = []
            text = ""
            i = 0
            sentences = [data[i:i+99] for i in range(0,len(data),99)]
            for sentence in tqdm(sentences):
                text = ""
                for value in sentence:
                    text = text + value + "\n"
                doc = await upload_message(text, os.path.splitext(os.path.basename(filepath))[0] + "_" + str(i))
                documents.append(doc)
                i = i + 1

            for document in tqdm(documents):
                await embed_message(document)

    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

async def start_no_split(filepath):
    try:
        with open(filepath, 'rt') as f:
            sentences = f.readlines()
            text = ""
            
            for value in sentences:
                text = text + value + "\n"
            document = await upload_message(text, os.path.splitext(os.path.basename(filepath))[0])

            await embed_message(document)

    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

async def start_single(filepath):
    try:
        with open(filepath, 'rt') as f:
            data = f.readlines()
            documents = []
            i = 0
            for value in tqdm(data):
                doc = await upload_message(value, os.path.splitext(os.path.basename(filepath))[0] + "_" + str(i))
                documents.append(doc)
                i = i + 1

            for document in tqdm(documents):
                await embed_message(document)

    except Exception as e:
        exc_type, exc_obj, exc_tb = sys.exc_info()
        fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
        logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

if not sys.argv[1].endswith(".txt"):
    raise Exception("only txt file is supported")
else:
    asyncio.run(start_no_split(sys.argv[1]))