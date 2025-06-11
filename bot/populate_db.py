
import database
from os.path import join, dirname

dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)
with open(join(dirname(__file__), 'config/sentences.txt'), 'rt') as f:
    data = f.readlines()
for line in data:
    database.insert_sentence(dbms,line.strip())