
import database
from os.path import join, dirname
from tqdm import tqdm

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)
with open(join(dirname(__file__), 'config/sentences.txt'), 'rt') as f:
    data = f.readlines()
for line in tqdm(data):
    database.insert_sentence(dbms,line.strip())