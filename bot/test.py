import database
import random


dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)
full = random.choice(database.select_all_sentence(dbms))
like = random.choice(database.select_like_sentence(dbms, "test"))

print("done")