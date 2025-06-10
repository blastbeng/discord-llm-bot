import database


dbms = database.Database(database.SQLITE, dbname='discord-bot.sqlite3')
database.create_db_tables(dbms)
full = database.select_all_sentence(dbms)
like = database.select_like_sentence(dbms, "test")

print("done")